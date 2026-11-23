//! Headless backend: outputs with no screen behind them.
//!
//! The unit tests use it bare (no renderer, frames only pretend to be presented). With
//! `ARBOR_BACKEND=headless` it is a full session for test rigs, which need no host compositor:
//! a GPU renderer (dmabuf import, so EGL clients work), outputs from `ARBOR_HEADLESS_OUTPUTS`
//! drawn into offscreen buffers (what screencopy and screenshots show), and a 60 Hz estimated
//! vblank for frame callbacks and animations.
//!
//! `ARBOR_HEADLESS_OUTPUTS`: comma-separated `[NAME=]WIDTHxHEIGHT`, e.g. `DP-1=5120x2160` (a
//! name lets the output config of a real monitor apply). Default `headless-1=1920x1080`.
//! The GPU: `ARBOR_HEADLESS_RENDER_NODE` (e.g. `/dev/dri/renderD128`), else the config's
//! `debug { render-drm-device }`, else Mesa's choice (`DRI_PRIME` applies).

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use arbor_config::{Config, OutputName};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::native::EGLSurfacelessDisplay;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, ImportDma, Offscreen};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::Size;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal};
use smithay::wayland::presentation::Refresh;

use super::{IpcOutputMap, OutputId, RenderResult};
use crate::arbor::{Arbor, RedrawState};
use crate::render_helpers::{resources, shaders, RenderCtx, RenderTarget};
use crate::utils::{get_monotonic_time, logical_output};

const REFRESH: Duration = Duration::from_micros(16_667);

pub struct Headless {
    renderer: Option<GlesRenderer>,
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,
    /// Set for a session (`ARBOR_BACKEND=headless`), not in unit tests.
    live: Option<Live>,
}

struct Live {
    config: Rc<RefCell<Config>>,
    outputs: Vec<(String, (u16, u16))>,
    targets: HashMap<Output, Target>,
    dmabuf_global: Option<DmabufGlobal>,
}

/// An output's offscreen framebuffer.
struct Target {
    texture: GlesTexture,
    size: Size<i32, smithay::utils::Physical>,
    damage_tracker: OutputDamageTracker,
}

/// Whether `ARBOR_BACKEND=headless` asks for a headless session.
pub fn requested() -> bool {
    std::env::var("ARBOR_BACKEND").is_ok_and(|v| v == "headless")
}

fn parse_outputs(spec: &str) -> anyhow::Result<Vec<(String, (u16, u16))>> {
    let mut outputs = Vec::new();
    for (i, item) in spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let (name, size) = match item.split_once('=') {
            Some((name, size)) => (name.to_owned(), size),
            None => (format!("headless-{}", i + 1), item),
        };
        let (w, h) = size
            .split_once('x')
            .with_context(|| format!("output {item:?}: expected WIDTHxHEIGHT"))?;
        let w: u16 = w
            .parse()
            .with_context(|| format!("output {item:?}: bad width"))?;
        let h: u16 = h
            .parse()
            .with_context(|| format!("output {item:?}: bad height"))?;
        anyhow::ensure!(w > 0 && h > 0, "output {item:?}: empty size");
        outputs.push((name, (w, h)));
    }
    anyhow::ensure!(!outputs.is_empty(), "no outputs in {spec:?}");
    Ok(outputs)
}

impl Headless {
    pub fn new() -> Self {
        Self {
            renderer: None,
            ipc_outputs: Default::default(),
            live: None,
        }
    }

    /// A headless session (`ARBOR_BACKEND=headless`): renders, imports dmabufs, paces frames.
    pub fn new_session(config: Rc<RefCell<Config>>) -> anyhow::Result<Self> {
        let spec = std::env::var("ARBOR_HEADLESS_OUTPUTS")
            .unwrap_or_else(|_| "headless-1=1920x1080".to_owned());
        let outputs = parse_outputs(&spec).context("error parsing ARBOR_HEADLESS_OUTPUTS")?;

        let mut headless = Self::new();
        headless.live = Some(Live {
            config,
            outputs,
            targets: HashMap::new(),
            dmabuf_global: None,
        });
        headless.add_renderer()?;
        Ok(headless)
    }

    pub fn init(&mut self, arbor: &mut Arbor) {
        let Some(live) = &mut self.live else {
            return;
        };
        let renderer = self.renderer.as_mut().unwrap();

        let config = live.config.borrow();
        if let Some(src) = config.animations.window_resize.custom_shader.as_deref() {
            shaders::set_custom_resize_program(renderer, Some(src));
        }
        if let Some(src) = config.animations.window_close.custom_shader.as_deref() {
            shaders::set_custom_close_program(renderer, Some(src));
        }
        if let Some(src) = config.animations.window_open.custom_shader.as_deref() {
            shaders::set_custom_open_program(renderer, Some(src));
        }
        drop(config);
        arbor.update_shaders();

        // linux-dmabuf with feedback naming our render node, so clients' EGL picks the same GPU.
        let feedback = EGLDevice::device_for_display(renderer.egl_context().display())
            .context("error getting EGL device")
            .and_then(|device| {
                device
                    .try_get_render_node()
                    .context("error getting EGL device render node")?
                    .context("EGL device has no render node")
            })
            .and_then(|node| {
                DmabufFeedbackBuilder::new(node.dev_id(), renderer.dmabuf_formats())
                    .build()
                    .context("error building dmabuf feedback")
            });
        let global = match feedback {
            Ok(feedback) => arbor
                .dmabuf_state
                .create_global_with_default_feedback::<crate::arbor::State>(
                    &arbor.display_handle,
                    &feedback,
                ),
            Err(err) => {
                debug!("dmabuf feedback unavailable, falling back to v3: {err:?}");
                arbor.dmabuf_state.create_global::<crate::arbor::State>(
                    &arbor.display_handle,
                    renderer.dmabuf_formats(),
                )
            }
        };
        live.dmabuf_global = Some(global);

        let outputs = live.outputs.clone();
        for (i, (name, size)) in outputs.into_iter().enumerate() {
            self.add_named_output(arbor, name, (i + 1).to_string(), size);
        }
    }

    pub fn add_renderer(&mut self) -> anyhow::Result<()> {
        if self.renderer.is_some() {
            error!("add_renderer: renderer must not already exist");
            return Ok(());
        }

        let node = std::env::var_os("ARBOR_HEADLESS_RENDER_NODE")
            .map(PathBuf::from)
            .or_else(|| {
                let live = self.live.as_ref()?;
                live.config.borrow().debug.render_drm_device.clone()
            });

        let mut renderer = unsafe {
            let display = match node {
                Some(node) => {
                    let device = EGLDevice::enumerate()
                        .context("error enumerating EGL devices")?
                        .find(|device| {
                            device
                                .try_get_render_node()
                                .ok()
                                .flatten()
                                .is_some_and(|n| {
                                    n.dev_path().is_some_and(|p| {
                                        p == node
                                            || std::fs::canonicalize(&node).is_ok_and(|c| c == p)
                                    })
                                })
                        })
                        .with_context(|| format!("no EGL device for {}", node.display()))?;
                    EGLDisplay::new(device).context("error creating EGL display")?
                }
                None => {
                    EGLDisplay::new(EGLSurfacelessDisplay).context("error creating EGL display")?
                }
            };
            let context = EGLContext::new(&display).context("error creating EGL context")?;
            GlesRenderer::new(context).context("error creating renderer")?
        };

        resources::init(&mut renderer);
        shaders::init(&mut renderer);

        self.renderer = Some(renderer);
        Ok(())
    }

    pub fn add_output(&mut self, arbor: &mut Arbor, n: u8, size: (u16, u16)) {
        self.add_named_output(arbor, format!("headless-{n}"), n.to_string(), size);
    }

    fn add_named_output(
        &mut self,
        arbor: &mut Arbor,
        connector: String,
        serial: String,
        size: (u16, u16),
    ) {
        let make = "arbor".to_string();
        let model = "headless".to_string();

        let output = Output::new(
            connector.clone(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: make.clone(),
                model: model.clone(),
                serial_number: serial.clone(),
            },
        );

        let mode = Mode {
            size: Size::from((i32::from(size.0), i32::from(size.1))),
            refresh: 60_000,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);

        output.user_data().insert_if_missing(|| OutputName {
            connector,
            make: Some(make),
            model: Some(model),
            serial: Some(serial),
        });

        let physical_properties = output.physical_properties();
        self.ipc_outputs.lock().unwrap().insert(
            OutputId::next(),
            arbor_ipc::Output {
                name: output.name(),
                make: physical_properties.make,
                model: physical_properties.model,
                serial: None,
                physical_size: None,
                modes: vec![arbor_ipc::Mode {
                    width: size.0,
                    height: size.1,
                    refresh_rate: 60_000,
                    is_preferred: true,
                }],
                current_mode: Some(0),
                is_custom_mode: true,
                vrr_supported: false,
                vrr_enabled: false,
                logical: Some(logical_output(&output)),
            },
        );

        let refresh = self.live.is_some().then_some(REFRESH);
        arbor.add_output(output, refresh, false);
    }

    pub fn seat_name(&self) -> String {
        "headless".to_owned()
    }

    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        self.renderer.as_mut().map(f)
    }

    pub fn render(&mut self, arbor: &mut Arbor, output: &Output) -> RenderResult {
        if self.live.is_some() {
            return self.render_session(arbor, output);
        }

        let states = RenderElementStates::default();
        let mut presentation_feedbacks = arbor.take_presentation_feedbacks(output, &states);
        presentation_feedbacks.presented::<_, smithay::utils::Monotonic>(
            get_monotonic_time(),
            Refresh::Unknown,
            0,
            wp_presentation_feedback::Kind::empty(),
        );

        let output_state = arbor.output_state.get_mut(output).unwrap();
        match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => (),
            RedrawState::WaitingForVBlank { .. } => unreachable!(),
            RedrawState::WaitingForEstimatedVBlank(_) => unreachable!(),
            RedrawState::WaitingForEstimatedVBlankAndQueued(_) => unreachable!(),
        }

        output_state.frame_callback_sequence = output_state.frame_callback_sequence.wrapping_add(1);

        // FIXME: request redraw on unfinished animations remain

        RenderResult::Submitted
    }

    /// Renders into the output's offscreen buffer, then waits out an estimated vblank (60 Hz)
    /// before frame callbacks and the next frame, like a monitor would pace them.
    fn render_session(&mut self, arbor: &mut Arbor, output: &Output) -> RenderResult {
        let _span = tracy_client::span!("Headless::render");
        let live = self.live.as_mut().unwrap();
        let renderer = self.renderer.as_mut().unwrap();

        let stats_start = crate::frame_stats::enabled().then(std::time::Instant::now);
        let ctx = RenderCtx {
            renderer: &mut *renderer,
            target: RenderTarget::Output,
            xray: None,
        };
        let elements = arbor.render_to_vec(ctx, output, true);

        let size = output.current_mode().map(|m| m.size).unwrap_or_default();
        if live.targets.get(output).is_none_or(|t| t.size != size) {
            let buffer_size = Size::from((size.w.max(1), size.h.max(1)));
            match Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, buffer_size) {
                Ok(texture) => {
                    live.targets.insert(
                        output.clone(),
                        Target {
                            texture,
                            size,
                            damage_tracker: OutputDamageTracker::from_output(output),
                        },
                    );
                }
                Err(err) => {
                    warn!("error creating headless framebuffer: {err:?}");
                    return RenderResult::Skipped;
                }
            }
        }
        let target = live.targets.get_mut(output).unwrap();

        let res = match renderer.bind(&mut target.texture) {
            Ok(mut framebuffer) => target
                .damage_tracker
                .render_output(renderer, &mut framebuffer, 0, &elements, [0.; 4])
                .map(|res| (res.damage.is_some(), res.states))
                .map_err(|err| anyhow::anyhow!("{err:?}")),
            Err(err) => Err(anyhow::anyhow!(
                "error binding headless framebuffer: {err:?}"
            )),
        };
        let (damaged, states) = match res {
            Ok(v) => v,
            Err(err) => {
                warn!("headless render failed: {err:?}");
                return RenderResult::Skipped;
            }
        };

        if let Some(start) = stats_start {
            crate::frame_stats::rendered(&output.name(), start.elapsed(), elements.len(), !damaged);
        }
        arbor.update_primary_scanout_output(output, &states);

        let rv = if damaged {
            let mut feedbacks = arbor.take_presentation_feedbacks(output, &states);
            feedbacks.presented::<_, smithay::utils::Monotonic>(
                get_monotonic_time() + REFRESH,
                Refresh::fixed(REFRESH),
                0,
                wp_presentation_feedback::Kind::empty(),
            );
            RenderResult::Submitted
        } else {
            RenderResult::NoDamage
        };

        queue_vblank_timer(arbor, output.clone());
        rv
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        match renderer.import_dmabuf(dmabuf, None) {
            Ok(_texture) => true,
            Err(err) => {
                debug!("error importing dmabuf: {err:?}");
                false
            }
        }
    }

    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}

/// After a frame: wait a refresh interval (keeping a timer already running), as the TTY
/// backend does when it has to estimate the vblank.
fn queue_vblank_timer(arbor: &mut Arbor, output: Output) {
    let output_state = arbor.output_state.get_mut(&output).unwrap();
    match mem::take(&mut output_state.redraw_state) {
        RedrawState::Queued => (),
        RedrawState::WaitingForEstimatedVBlank(token)
        | RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
            output_state.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
            return;
        }
        RedrawState::Idle | RedrawState::WaitingForVBlank { .. } => unreachable!(),
    }

    let token = arbor
        .event_loop
        .insert_source(Timer::from_duration(REFRESH), move |_, _, state| {
            on_vblank_timer(&mut state.arbor, &output);
            TimeoutAction::Drop
        })
        .unwrap();
    output_state.redraw_state = RedrawState::WaitingForEstimatedVBlank(token);
}

fn on_vblank_timer(arbor: &mut Arbor, output: &Output) {
    let Some(output_state) = arbor.output_state.get_mut(output) else {
        return;
    };
    output_state.frame_callback_sequence = output_state.frame_callback_sequence.wrapping_add(1);

    match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
        RedrawState::WaitingForEstimatedVBlank(_) => (),
        RedrawState::WaitingForEstimatedVBlankAndQueued(_) => {
            output_state.redraw_state = RedrawState::Queued;
            return;
        }
        other => {
            output_state.redraw_state = other;
            return;
        }
    }

    if output_state.unfinished_animations_remain {
        arbor.queue_redraw(output);
    } else {
        arbor.send_frame_callbacks(output);
    }
}

impl Default for Headless {
    fn default() -> Self {
        Self::new()
    }
}
