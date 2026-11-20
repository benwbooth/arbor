//! arbor: CRTC colour pipeline (DEGAMMA_LUT, CTM, GAMMA_LUT) for EDID gamut mapping and HDR.
//!
//! All three properties are owned by [`GammaProps`], which also owns the wlr-gamma-control
//! ramp, so the gamma-control path and the gamut mapping never fight over GAMMA_LUT. With a
//! mapping active, GAMMA_LUT receives linear light, so the client ramp is composed after the
//! inverse sRGB EOTF.
//!
//! HDR is done entirely in the CRTC pipeline too: DEGAMMA = sRGB EOTF, CTM = sRGB -> BT.2020,
//! GAMMA_LUT = PQ with sRGB white at the configured SDR brightness; the connector gets
//! `Colorspace` = BT2020_RGB, `HDR_OUTPUT_METADATA` (PQ) and `max bpc` >= 10. The plane
//! contents stay 8-bit sRGB; the pipeline runs at higher precision and the link at 10 bpc.

use std::num::NonZeroU64;
use std::os::fd::AsFd as _;

use anyhow::{ensure, Context as _};
use bytemuck::cast_slice;
use niri_config::output::{ColorProfile, Hdr};
use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::{crtc, property, AtomicCommitFlags, Device as _};

use super::{get_drm_property, reset_hdr, ConnectorProperties, GammaProps};
use crate::backend::color::{self, Chromaticities, OutputTransfer, Xy};

/// Handles of the CRTC colour properties besides GAMMA_LUT.
pub(super) struct ColorProps {
    degamma_lut: property::Handle,
    degamma_lut_size: property::Handle,
    ctm: property::Handle,
    previous_degamma_blob: Option<NonZeroU64>,
    previous_ctm_blob: Option<NonZeroU64>,
}

/// Colour state kept alongside the gamma properties.
#[derive(Default)]
pub(super) struct ColorState {
    pub props: Option<ColorProps>,
    /// Last ramp set by a gamma-control client (encoded in, encoded out).
    pub client_ramp: Option<Vec<u16>>,
    /// Panel chromaticities from its EDID.
    pub edid_chroma: Option<Chromaticities>,
    /// The mapping currently programmed, if any.
    pub active: Option<Mapping>,
    /// The configured profile.
    pub profile: ColorProfile,
    /// The configured HDR settings, if HDR is enabled.
    pub hdr: Option<Hdr>,
}

/// A programmed CRTC colour mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Mapping {
    pub ctm: [u64; 9],
    pub transfer: OutputTransfer,
}

impl ColorState {
    pub fn hdr_active(&self) -> bool {
        matches!(
            self.active,
            Some(Mapping {
                transfer: OutputTransfer::Pq { .. },
                ..
            })
        )
    }
}

impl ColorProps {
    pub fn new(device: &DrmDevice, crtc: crtc::Handle) -> anyhow::Result<Self> {
        let mut degamma_lut = None;
        let mut degamma_lut_size = None;
        let mut ctm = None;

        let props = device
            .get_properties(crtc)
            .context("error getting properties")?;
        for (prop, _) in props {
            let Ok(info) = device.get_property(prop) else {
                continue;
            };
            match info.name().to_bytes() {
                b"DEGAMMA_LUT" => degamma_lut = Some(prop),
                b"DEGAMMA_LUT_SIZE" => degamma_lut_size = Some(prop),
                b"CTM" => ctm = Some(prop),
                _ => (),
            }
        }

        Ok(Self {
            degamma_lut: degamma_lut.context("missing DEGAMMA_LUT property")?,
            degamma_lut_size: degamma_lut_size.context("missing DEGAMMA_LUT_SIZE property")?,
            ctm: ctm.context("missing CTM property")?,
            previous_degamma_blob: None,
            previous_ctm_blob: None,
        })
    }
}

/// Reads the panel chromaticities from the EDID base block.
pub(super) fn edid_chromaticities(info: &libdisplay_info::info::Info) -> Option<Chromaticities> {
    let c = info.edid()?.chromaticity_coords();
    let xy = |x: f32, y: f32| Xy {
        x: f64::from(x),
        y: f64::from(y),
    };
    Some(Chromaticities {
        red: xy(c.red_x, c.red_y),
        green: xy(c.green_x, c.green_y),
        blue: xy(c.blue_x, c.blue_y),
        white: xy(c.white_x, c.white_y),
    })
}

/// Computes the CTM for a panel; `None` if the chromaticities are unusable.
pub(super) fn edid_ctm(chroma: &Chromaticities) -> Option<[u64; 9]> {
    color::srgb_to_panel(chroma).map(|m| color::to_drm_ctm(&m))
}

const DRM_MODE_COLORIMETRY_BT2020_RGB: u64 = 9;

/// Turns on HDR signalling on the connector: `max bpc` >= 10, `Colorspace` = BT2020_RGB and
/// PQ `HDR_OUTPUT_METADATA`.
pub(super) fn set_hdr_signalling(
    props: &ConnectorProperties,
    hdr: &Hdr,
    mastering: Option<&Chromaticities>,
) -> anyhow::Result<()> {
    let device = props.device;

    if let Ok((info, value)) = props.find(c"max bpc") {
        if let property::ValueType::UnsignedRange(_, max) = info.value_type() {
            let want = 10.min(max);
            if *value < want {
                if let Err(err) = device.set_property(props.connector, info.handle(), want) {
                    warn!("error setting max bpc to {want}: {err:?}");
                }
            }
        }
    }

    let (info, _) = props.find(c"Colorspace")?;
    let property::ValueType::Enum(values) = info.value_type() else {
        anyhow::bail!("wrong Colorspace property type");
    };
    let bt2020 = values
        .values()
        .1
        .iter()
        .find(|v| v.name().to_bytes() == b"BT2020_RGB")
        .map_or(DRM_MODE_COLORIMETRY_BT2020_RGB, |v| v.value());
    device
        .set_property(props.connector, info.handle(), bt2020)
        .context("error setting Colorspace")?;

    let (info, _) = props.find(c"HDR_OUTPUT_METADATA")?;
    let metadata =
        color::hdr_output_metadata(mastering.unwrap_or(&color::BT2020), hdr.max_luminance(), 0.);
    let blob = create_blob(device, &metadata)?;
    let res = device
        .set_property(props.connector, info.handle(), blob.get())
        .context("error setting HDR_OUTPUT_METADATA");
    // The connector state holds its own reference.
    destroy_blob(device, Some(blob));
    res
}

fn lut_bytes(lut: &[u16]) -> Vec<u8> {
    #[allow(non_camel_case_types)]
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct drm_color_lut {
        red: u16,
        green: u16,
        blue: u16,
        reserved: u16,
    }

    let n = lut.len() / 3;
    let (r, rest) = lut.split_at(n);
    let (g, b) = rest.split_at(n);
    let data = (0..n)
        .map(|i| drm_color_lut {
            red: r[i],
            green: g[i],
            blue: b[i],
            reserved: 0,
        })
        .collect::<Vec<_>>();
    cast_slice(&data).to_vec()
}

fn create_blob(device: &DrmDevice, data: &[u8]) -> anyhow::Result<NonZeroU64> {
    let mut data = data.to_vec();
    let blob = drm_ffi::mode::create_property_blob(device.as_fd(), &mut data)
        .context("error creating property blob")?;
    NonZeroU64::new(u64::from(blob.blob_id)).context("kernel returned blob id 0")
}

fn destroy_blob(device: &DrmDevice, blob: Option<NonZeroU64>) {
    if let Some(blob) = blob {
        if let Err(err) = device.destroy_property_blob(blob.get()) {
            warn!("error destroying property blob: {err:?}");
        }
    }
}

fn blob_value(blob: Option<NonZeroU64>) -> property::RawValue {
    property::Value::Blob(blob.map_or(0, NonZeroU64::get)).into()
}

/// Sets several CRTC properties together: one atomic commit if possible, so the screen never
/// shows a half-programmed pipeline.
fn set_crtc_props(
    device: &DrmDevice,
    crtc: crtc::Handle,
    props: &[(property::Handle, property::RawValue)],
) -> anyhow::Result<()> {
    if device.is_atomic() {
        let mut req = AtomicModeReq::new();
        for &(prop, value) in props {
            req.add_raw_property(crtc.into(), prop, value);
        }
        match device.atomic_commit(AtomicCommitFlags::empty(), req) {
            Ok(()) => return Ok(()),
            // Fall back to the per-property path that niri uses for GAMMA_LUT.
            Err(err) => debug!("atomic colour commit failed, setting one by one: {err:?}"),
        }
    }
    for &(prop, value) in props {
        device
            .set_property(crtc, prop, value)
            .context("error setting colour property")?;
    }
    Ok(())
}

impl GammaProps {
    /// The GAMMA_LUT contents for the current state, or `None` for "no LUT".
    pub(super) fn effective_gamma_lut(
        &self,
        device: &DrmDevice,
    ) -> anyhow::Result<Option<Vec<u16>>> {
        self.gamma_lut_for(device, self.color.active.map(|m| m.transfer))
    }

    fn gamma_lut_for(
        &self,
        device: &DrmDevice,
        transfer: Option<OutputTransfer>,
    ) -> anyhow::Result<Option<Vec<u16>>> {
        let ramp = self.color.client_ramp.as_deref();
        let Some(transfer) = transfer else {
            return Ok(ramp.map(<[u16]>::to_vec));
        };
        let size = self
            .gamma_size(device)
            .context("error getting gamma size")? as usize;
        Ok(Some(color::output_lut(size, ramp, transfer)))
    }

    /// The mapping that the configuration asks for, if it can be honoured.
    fn wanted_mapping(&self) -> Option<Mapping> {
        self.color.props.as_ref()?;
        if let Some(hdr) = &self.color.hdr {
            let m = color::srgb_to_panel(&color::BT2020)?;
            return Some(Mapping {
                ctm: color::to_drm_ctm(&m),
                transfer: OutputTransfer::Pq {
                    sdr_white: hdr.sdr_brightness(),
                },
            });
        }
        match self.color.profile {
            ColorProfile::Srgb => None,
            ColorProfile::Edid => Some(Mapping {
                ctm: edid_ctm(self.color.edid_chroma.as_ref()?)?,
                transfer: OutputTransfer::Srgb,
            }),
        }
    }

    /// Applies the colour configuration to the CRTC and the connector. `force` reprograms even
    /// if nothing changed (used on startup to clear state left behind by a previous DRM
    /// master).
    pub(super) fn set_color_config(
        &mut self,
        props: &ConnectorProperties,
        profile: ColorProfile,
        hdr: Option<Hdr>,
        force: bool,
    ) -> anyhow::Result<()> {
        let device = props.device;
        self.color.profile = profile;
        self.color.hdr = hdr;
        let wanted = self.wanted_mapping();
        if wanted.is_none() && (hdr.is_some() || profile == ColorProfile::Edid) {
            if self.color.props.is_none() {
                warn!("color-profile/hdr: CRTC has no DEGAMMA_LUT/CTM, ignoring");
            } else {
                warn!("color-profile edid: no usable EDID chromaticities, ignoring");
            }
        }
        if !force && wanted == self.color.active {
            return Ok(());
        }

        let was_hdr = self.color.hdr_active();
        let wants_hdr = matches!(
            wanted,
            Some(Mapping {
                transfer: OutputTransfer::Pq { .. },
                ..
            })
        );

        // Leaving HDR: drop the signalling first.
        if was_hdr && !wants_hdr {
            if let Err(err) = reset_hdr(props) {
                warn!("error resetting HDR signalling: {err:?}");
            }
        }

        self.program_color(device, wanted)?;

        if wants_hdr {
            let hdr = self.color.hdr.unwrap();
            if let Err(err) = set_hdr_signalling(props, &hdr, self.color.edid_chroma.as_ref()) {
                warn!("error enabling HDR signalling, falling back to SDR: {err:?}");
                let _ = reset_hdr(props);
                self.color.hdr = None;
                let fallback = self.wanted_mapping();
                self.program_color(device, fallback)?;
            }
        }
        Ok(())
    }

    /// Re-enables HDR signalling after something reset the connector (VT switch).
    pub(super) fn restore_hdr_signalling(&self, props: &ConnectorProperties) {
        if !self.color.hdr_active() {
            return;
        }
        if let Some(hdr) = &self.color.hdr {
            if let Err(err) = set_hdr_signalling(props, hdr, self.color.edid_chroma.as_ref()) {
                warn!("error restoring HDR signalling: {err:?}");
            }
        }
    }

    fn program_color(
        &mut self,
        device: &DrmDevice,
        mapping: Option<Mapping>,
    ) -> anyhow::Result<()> {
        let _span = tracy_client::span!("GammaProps::program_color");

        let Some(props) = &self.color.props else {
            // No colour pipeline: only GAMMA_LUT exists, nothing to do beyond it.
            self.color.active = None;
            return Ok(());
        };
        let (degamma_prop, degamma_size_prop, ctm_prop) =
            (props.degamma_lut, props.degamma_lut_size, props.ctm);

        let mut new_blobs = Vec::new();
        let mut result = || {
            let (degamma, ctm_blob) = if let Some(Mapping { ctm, .. }) = &mapping {
                let size = get_drm_property(device, self.crtc, degamma_size_prop)
                    .context("missing DEGAMMA_LUT_SIZE")? as usize;
                ensure!(size >= 2, "DEGAMMA_LUT_SIZE too small");
                let degamma = create_blob(device, &lut_bytes(&color::degamma_lut(size)))?;
                new_blobs.push(degamma);
                let ctm_blob = create_blob(device, cast_slice(ctm))?;
                new_blobs.push(ctm_blob);
                (Some(degamma), Some(ctm_blob))
            } else {
                (None, None)
            };

            let gamma_blob = match self.gamma_lut_for(device, mapping.map(|m| m.transfer))? {
                Some(lut) => {
                    let size = self.gamma_size(device)? as usize;
                    ensure!(lut.len() == size * 3, "wrong gamma length");
                    let blob = create_blob(device, &lut_bytes(&lut))?;
                    new_blobs.push(blob);
                    Some(blob)
                }
                None => None,
            };

            set_crtc_props(
                device,
                self.crtc,
                &[
                    (degamma_prop, blob_value(degamma)),
                    (ctm_prop, blob_value(ctm_blob)),
                    (self.gamma_lut, blob_value(gamma_blob)),
                ],
            )?;
            Ok((degamma, ctm_blob, gamma_blob))
        };
        let result = result();

        let (degamma, ctm_blob, gamma_blob) = match result {
            Ok(x) => x,
            Err(err) => {
                for blob in new_blobs {
                    destroy_blob(device, Some(blob));
                }
                return Err(err);
            }
        };

        let props = self.color.props.as_mut().unwrap();
        destroy_blob(
            device,
            std::mem::replace(&mut props.previous_degamma_blob, degamma),
        );
        destroy_blob(
            device,
            std::mem::replace(&mut props.previous_ctm_blob, ctm_blob),
        );
        destroy_blob(
            device,
            std::mem::replace(&mut self.previous_blob, gamma_blob),
        );
        self.color.active = mapping;

        if let Some(m) = mapping {
            debug!(
                "programmed colour mapping {:?}, CTM {:?}",
                m.transfer,
                color::from_drm_ctm(&m.ctm)
            );
        }
        Ok(())
    }

    /// Re-sets our DEGAMMA_LUT and CTM blobs (e.g. after a VT switch); GAMMA_LUT is restored
    /// separately by `restore_gamma`.
    pub(super) fn restore_color(&self, device: &DrmDevice) -> anyhow::Result<()> {
        let Some(props) = &self.color.props else {
            return Ok(());
        };
        set_crtc_props(
            device,
            self.crtc,
            &[
                (props.degamma_lut, blob_value(props.previous_degamma_blob)),
                (props.ctm, blob_value(props.previous_ctm_blob)),
                (self.gamma_lut, blob_value(self.previous_blob)),
            ],
        )
    }
}
