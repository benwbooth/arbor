//! arbor: colour management math for the KMS CRTC colour pipeline.
//!
//! The CRTC pipeline is `DEGAMMA_LUT -> CTM -> GAMMA_LUT`. To map sRGB content onto a
//! wide-gamut panel we program:
//!
//! - `DEGAMMA_LUT` = sRGB EOTF (encoded -> linear),
//! - `CTM` = linear sRGB (BT.709, D65) -> linear panel-native RGB,
//! - `GAMMA_LUT` = inverse sRGB EOTF (linear -> encoded), optionally composed with a
//!   wlr-gamma-control client ramp (night light) which expects encoded input.

/// CIE 1931 xy chromaticity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xy {
    pub x: f64,
    pub y: f64,
}

/// Primaries and white point of an RGB colour space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chromaticities {
    pub red: Xy,
    pub green: Xy,
    pub blue: Xy,
    pub white: Xy,
}

/// BT.709 / sRGB primaries with D65 white.
pub const SRGB: Chromaticities = Chromaticities {
    red: Xy { x: 0.64, y: 0.33 },
    green: Xy { x: 0.30, y: 0.60 },
    blue: Xy { x: 0.15, y: 0.06 },
    white: Xy {
        x: 0.3127,
        y: 0.3290,
    },
};

pub type Mat3 = [[f64; 3]; 3];

const IDENTITY: Mat3 = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mul_vec(a: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

fn det(m: &Mat3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn invert(m: &Mat3) -> Option<Mat3> {
    let d = det(m);
    if !d.is_finite() || d.abs() < 1e-12 {
        return None;
    }
    let c =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    Some([
        [c(1, 1, 2, 2) / d, -c(0, 1, 2, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 0, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 1, 2) / d],
        [c(1, 0, 2, 1) / d, -c(0, 0, 2, 1) / d, c(0, 0, 1, 1) / d],
    ])
}

fn xy_to_xyz(c: Xy) -> [f64; 3] {
    [c.x / c.y, 1., (1. - c.x - c.y) / c.y]
}

/// Matrix converting linear RGB in the given space to CIE XYZ (Y of white = 1).
pub fn rgb_to_xyz(c: &Chromaticities) -> Option<Mat3> {
    for p in [c.red, c.green, c.blue, c.white] {
        if !(p.x.is_finite() && p.y.is_finite()) || p.x <= 0. || p.y <= 0. || p.x + p.y > 1. {
            return None;
        }
    }

    let [r, g, b] = [c.red, c.green, c.blue].map(xy_to_xyz);
    let prim = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mul_vec(&invert(&prim)?, xy_to_xyz(c.white));
    if s.iter().any(|v| *v <= 0.) {
        // White point outside the primaries' triangle.
        return None;
    }
    Some([0, 1, 2].map(|i| [0, 1, 2].map(|j| prim[i][j] * s[j])))
}

/// Bradford chromatic adaptation from one white point to another.
pub fn bradford(from: Xy, to: Xy) -> Option<Mat3> {
    const B: Mat3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let b_inv = invert(&B)?;
    let src = mul_vec(&B, xy_to_xyz(from));
    let dst = mul_vec(&B, xy_to_xyz(to));
    let scale = [
        [dst[0] / src[0], 0., 0.],
        [0., dst[1] / src[1], 0.],
        [0., 0., dst[2] / src[2]],
    ];
    Some(mul(&b_inv, &mul(&scale, &B)))
}

/// Matrix converting linear sRGB to linear RGB of a display with the given primaries.
///
/// sRGB white (D65) is adapted (Bradford) to the panel's native white, so full white stays
/// full white (every row sums to 1) and nothing clips at white. Colours outside the panel
/// gamut produce out-of-range values which the hardware clamps after the CTM.
pub fn srgb_to_panel(panel: &Chromaticities) -> Option<Mat3> {
    let srgb_to_xyz = rgb_to_xyz(&SRGB)?;
    let xyz_to_panel = invert(&rgb_to_xyz(panel)?)?;
    let adapt = if (panel.white.x - SRGB.white.x).abs() < 1e-6
        && (panel.white.y - SRGB.white.y).abs() < 1e-6
    {
        IDENTITY
    } else {
        bradford(SRGB.white, panel.white)?
    };

    let mut m = mul(&xyz_to_panel, &mul(&adapt, &srgb_to_xyz));

    // Remove floating-point drift so white maps exactly to white.
    for row in &mut m {
        let sum: f64 = row.iter().sum();
        if sum.abs() < 1e-6 {
            return None;
        }
        for v in row.iter_mut() {
            *v /= sum;
        }
    }

    if m.iter().flatten().any(|v| !v.is_finite() || v.abs() > 8.) {
        return None;
    }
    Some(m)
}

/// Encodes a matrix as `struct drm_color_ctm`: row-major S31.32 sign-magnitude.
pub fn to_drm_ctm(m: &Mat3) -> [u64; 9] {
    let mut out = [0u64; 9];
    for (o, v) in out.iter_mut().zip(m.iter().flatten()) {
        let mag = (v.abs() * (1u64 << 32) as f64).round();
        let mag = (mag as u64) & !(1u64 << 63);
        *o = if *v < 0. && mag != 0 {
            mag | (1 << 63)
        } else {
            mag
        };
    }
    out
}

/// Decodes a `struct drm_color_ctm` (for tests and logging).
pub fn from_drm_ctm(ctm: &[u64; 9]) -> Mat3 {
    let mut m = [[0.; 3]; 3];
    for (v, raw) in m.iter_mut().flatten().zip(ctm) {
        let mag = (raw & !(1u64 << 63)) as f64 / (1u64 << 32) as f64;
        *v = if raw & (1 << 63) != 0 { -mag } else { mag };
    }
    m
}

/// sRGB EOTF: encoded -> linear.
pub fn srgb_eotf(e: f64) -> f64 {
    let e = e.clamp(0., 1.);
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB EOTF: linear -> encoded.
pub fn srgb_inv_eotf(l: f64) -> f64 {
    let l = l.clamp(0., 1.);
    if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1. / 2.4) - 0.055
    }
}

fn to_u16(v: f64) -> u16 {
    (v.clamp(0., 1.) * 65535.).round() as u16
}

/// One channel of a LUT with `size` entries sampling `f` over [0, 1].
fn lut_channel(size: usize, f: impl Fn(f64) -> f64) -> Vec<u16> {
    let denom = size.saturating_sub(1).max(1) as f64;
    (0..size).map(|i| to_u16(f(i as f64 / denom))).collect()
}

/// Linear interpolation into a ramp sampled uniformly over [0, 1].
fn sample(ramp: &[u16], x: f64) -> f64 {
    match ramp.len() {
        0 => x,
        1 => ramp[0] as f64 / 65535.,
        n => {
            let pos = x.clamp(0., 1.) * (n - 1) as f64;
            let i = (pos.floor() as usize).min(n - 2);
            let t = pos - i as f64;
            (ramp[i] as f64 * (1. - t) + ramp[i + 1] as f64 * t) / 65535.
        }
    }
}

/// `DEGAMMA_LUT` contents (R, G, B channels concatenated, `size` entries each).
pub fn degamma_lut(size: usize) -> Vec<u16> {
    let ch = lut_channel(size, srgb_eotf);
    [ch.as_slice(), &ch, &ch].concat()
}

/// `GAMMA_LUT` contents for a linear-light input (after DEGAMMA and CTM): the inverse sRGB
/// EOTF, followed by the gamma-control client ramp if any.
///
/// `client_ramp` is in wlr-gamma-control layout (R, G, B channels concatenated) with any
/// number of entries per channel; it expects encoded input, so it is applied after the
/// inverse EOTF.
pub fn gamma_lut(size: usize, client_ramp: Option<&[u16]>) -> Vec<u16> {
    match client_ramp {
        None => {
            let ch = lut_channel(size, srgb_inv_eotf);
            [ch.as_slice(), &ch, &ch].concat()
        }
        Some(ramp) => {
            let n = ramp.len() / 3;
            let (r, rest) = ramp.split_at(n);
            let (g, b) = rest.split_at(n);
            let mut out = Vec::with_capacity(size * 3);
            for ch in [&r[..n], &g[..n], &b[..n]] {
                out.extend(lut_channel(size, |l| sample(ch, srgb_inv_eotf(l))));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_mat_eq(a: &Mat3, b: &Mat3, tol: f64) {
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (a[i][j] - b[i][j]).abs() < tol,
                    "mismatch at [{i}][{j}]: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn srgb_to_xyz_matches_reference() {
        // IEC 61966-2-1 matrix.
        let expected = [
            [0.4124, 0.3576, 0.1805],
            [0.2126, 0.7152, 0.0722],
            [0.0193, 0.1192, 0.9505],
        ];
        assert_mat_eq(&rgb_to_xyz(&SRGB).unwrap(), &expected, 1e-3);
    }

    #[test]
    fn srgb_panel_is_identity() {
        assert_mat_eq(&srgb_to_panel(&SRGB).unwrap(), &IDENTITY, 1e-9);
    }

    #[test]
    fn srgb_to_display_p3() {
        // Well-known sRGB -> Display P3 (both D65) matrix.
        let p3 = Chromaticities {
            red: Xy { x: 0.680, y: 0.320 },
            green: Xy { x: 0.265, y: 0.690 },
            blue: Xy { x: 0.150, y: 0.060 },
            white: SRGB.white,
        };
        let expected = [
            [0.8225, 0.1774, 0.0000],
            [0.0332, 0.9669, 0.0000],
            [0.0171, 0.0724, 0.9108],
        ];
        assert_mat_eq(&srgb_to_panel(&p3).unwrap(), &expected, 1e-3);
    }

    #[test]
    fn srgb_to_bt2020() {
        let bt2020 = Chromaticities {
            red: Xy { x: 0.708, y: 0.292 },
            green: Xy { x: 0.170, y: 0.797 },
            blue: Xy { x: 0.131, y: 0.046 },
            white: SRGB.white,
        };
        // ITU-R BT.2087 matrix.
        let expected = [
            [0.6274, 0.3293, 0.0433],
            [0.0691, 0.9195, 0.0114],
            [0.0164, 0.0880, 0.8956],
        ];
        assert_mat_eq(&srgb_to_panel(&bt2020).unwrap(), &expected, 1e-3);
    }

    #[test]
    fn non_d65_white_maps_white_to_white() {
        let panel = Chromaticities {
            red: Xy { x: 0.676, y: 0.323 },
            green: Xy { x: 0.253, y: 0.681 },
            blue: Xy { x: 0.146, y: 0.053 },
            white: Xy {
                x: 0.3135,
                y: 0.3291,
            },
        };
        let m = srgb_to_panel(&panel).unwrap();
        let w = mul_vec(&m, [1., 1., 1.]);
        for v in w {
            assert!((v - 1.).abs() < 1e-9);
        }
        // Wider gamut: diagonal below 1.
        for (i, row) in m.iter().enumerate() {
            assert!(row[i] < 1. && row[i] > 0.5);
        }
    }

    #[test]
    fn bradford_identity_and_d50() {
        assert_mat_eq(&bradford(SRGB.white, SRGB.white).unwrap(), &IDENTITY, 1e-9);
        // Lindbloom's D65 -> D50 Bradford matrix.
        let d50 = Xy {
            x: 0.34567,
            y: 0.35850,
        };
        let expected = [
            [1.0478, 0.0229, -0.0501],
            [0.0295, 0.9905, -0.0171],
            [-0.0092, 0.0151, 0.7519],
        ];
        assert_mat_eq(&bradford(SRGB.white, d50).unwrap(), &expected, 1e-3);
    }

    #[test]
    fn invalid_chromaticities() {
        let mut c = SRGB;
        c.red = Xy { x: 0., y: 0. };
        assert!(srgb_to_panel(&c).is_none());
        // Collinear primaries.
        let c = Chromaticities {
            red: Xy { x: 0.2, y: 0.2 },
            green: Xy { x: 0.3, y: 0.3 },
            blue: Xy { x: 0.4, y: 0.4 },
            white: SRGB.white,
        };
        assert!(srgb_to_panel(&c).is_none());
    }

    #[test]
    fn ctm_encoding() {
        let m = [[1., -0.5, 0.], [0.25, 2., -1.], [0., 0., 1.]];
        let raw = to_drm_ctm(&m);
        assert_eq!(raw[0], 1 << 32);
        assert_eq!(raw[1], (1 << 63) | (1 << 31));
        assert_eq!(raw[2], 0);
        assert_eq!(raw[3], 1 << 30);
        assert_eq!(raw[4], 2 << 32);
        assert_eq!(raw[5], (1 << 63) | (1 << 32));
        assert_eq!(from_drm_ctm(&raw), m);

        let p = srgb_to_panel(&Chromaticities {
            red: Xy { x: 0.68, y: 0.32 },
            green: Xy { x: 0.265, y: 0.69 },
            blue: Xy { x: 0.15, y: 0.06 },
            white: SRGB.white,
        })
        .unwrap();
        assert_mat_eq(&from_drm_ctm(&to_drm_ctm(&p)), &p, 1e-9);
    }

    #[test]
    fn transfer_functions_round_trip() {
        for i in 0..=1000 {
            let x = i as f64 / 1000.;
            assert!((srgb_inv_eotf(srgb_eotf(x)) - x).abs() < 1e-9);
        }
        assert!((srgb_eotf(0.5) - 0.2140).abs() < 1e-4);
    }

    #[test]
    fn degamma_then_gamma_is_identity() {
        let size = 4096;
        let de = degamma_lut(size);
        let ga = gamma_lut(size, None);
        assert_eq!(de.len(), size * 3);
        assert_eq!(ga.len(), size * 3);
        assert_eq!(de[0], 0);
        assert_eq!(de[size - 1], 65535);
        assert_eq!(ga[size - 1], 65535);
        // Every 8-bit input survives encoded -> linear -> encoded.
        for v in 0..=255u32 {
            let e = v as f64 / 255.;
            let lin = sample(&de[..size], e);
            let out = sample(&ga[..size], lin);
            assert!((out * 255. - v as f64).abs() < 0.5, "{v} -> {}", out * 255.);
        }
    }

    #[test]
    fn identity_client_ramp_composes_to_plain_gamma() {
        let n = 256;
        let ch: Vec<u16> = (0..n).map(|i| (i * 65535 / (n - 1)) as u16).collect();
        let ramp = [ch.as_slice(), &ch, &ch].concat();
        let plain = gamma_lut(4096, None);
        let composed = gamma_lut(4096, Some(&ramp));
        for (a, b) in plain.iter().zip(&composed) {
            assert!((*a as i32 - *b as i32).abs() <= 1);
        }
    }

    #[test]
    fn night_light_ramp_composes() {
        // Blue at half, red unchanged: like a warm night-light ramp.
        let n = 4096;
        let lin: Vec<u16> = (0..n).map(|i| (i * 65535 / (n - 1)) as u16).collect();
        let half: Vec<u16> = lin.iter().map(|v| v / 2).collect();
        let ramp = [lin.as_slice(), &lin, &half].concat();
        let out = gamma_lut(n, Some(&ramp));
        let (r, rest) = out.split_at(n);
        let (_g, b) = rest.split_at(n);
        for i in (0..n).step_by(97) {
            let expected = srgb_inv_eotf(i as f64 / (n - 1) as f64);
            assert!((r[i] as f64 / 65535. - expected).abs() < 1e-3);
            assert!((b[i] as f64 / 65535. - expected / 2.).abs() < 1e-3);
        }
    }

    /// LG UltraGear 5120x2160 OLED (GSM 40605) base-block chromaticities, as parsed from
    /// EDID bytes 25..=34 (10-bit values / 1024).
    #[test]
    fn lg_ultragear_matrix() {
        let panel = Chromaticities {
            red: Xy {
                x: 692. / 1024.,
                y: 331. / 1024.,
            },
            green: Xy {
                x: 259. / 1024.,
                y: 697. / 1024.,
            },
            blue: Xy {
                x: 150. / 1024.,
                y: 54. / 1024.,
            },
            white: Xy {
                x: 321. / 1024.,
                y: 337. / 1024.,
            },
        };
        let m = srgb_to_panel(&panel).unwrap();
        let w = mul_vec(&m, [1., 1., 1.]);
        for v in w {
            assert!((v - 1.).abs() < 1e-9);
        }
        // Roughly a DCI-P3-class panel.
        assert!(m[0][0] > 0.75 && m[0][0] < 0.9, "{m:?}");
        assert!(m[1][1] > 0.9 && m[1][1] < 1.0, "{m:?}");
    }
}
