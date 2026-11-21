//! Window capture (`hyprland_toplevel_export_manager_v1`): handler and rendering.

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;
use smithay::utils::{Point, Rectangle, Scale, Transform};
use smithay::wayland::shm;

use crate::arbor::{Arbor, State};
use crate::delegate_toplevel_export;
use crate::layout::LayoutElement as _;
use crate::protocols::toplevel_export::{
    ExportBuffer, ExportTarget, ToplevelExport, ToplevelExportHandler,
    ToplevelExportManagerState, EXPORT_FOURCC,
};
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{render_elements_to_dmabuf, render_to_vec, RenderCtx, RenderTarget};
use crate::window::Mapped;

fn target_for(mapped: &Mapped, output: Option<&smithay::output::Output>) -> ExportTarget {
    let scale = output.map_or(1., |o| o.current_scale().fractional_scale());
    let size = mapped
        .size()
        .to_f64()
        .to_physical_precise_round(scale);
    ExportTarget {
        window: mapped.id().get(),
        size,
        scale,
    }
}

impl ToplevelExportHandler for State {
    fn toplevel_export_state(&mut self) -> &mut ToplevelExportManagerState {
        &mut self.arbor.toplevel_export_state
    }

    fn export_target_for_handle(
        &mut self,
        handle: &ZwlrForeignToplevelHandleV1,
    ) -> Option<ExportTarget> {
        let surface = self
            .arbor
            .foreign_toplevel_state
            .surface_for_wlr_handle(handle)?;
        let (mapped, output) = self.arbor.layout.find_window_and_output(&surface)?;
        Some(target_for(mapped, output))
    }

    fn export_target_for_id(&mut self, id: u64) -> Option<ExportTarget> {
        let (mon, mapped) = self
            .arbor
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id().get() == id)?;
        Some(target_for(mapped, mon.map(|mon| mon.output())))
    }

    fn export_frame(&mut self, export: ToplevelExport) {
        if export.ignore_damage {
            self.backend.with_primary_renderer(|renderer| {
                if let Some(export) = self.arbor.render_toplevel_export(renderer, export) {
                    // Can't happen with ignore_damage; fail it.
                    drop(export);
                }
            });
        } else {
            self.arbor.toplevel_export_state.push(export);
            // Check for damage that happened since the previous copy on the next redraw.
            self.arbor.queue_redraw_all();
        }
    }
}
delegate_toplevel_export!(State);

impl Arbor {
    /// Renders queued window captures that have damage; called on every output redraw.
    pub fn render_pending_toplevel_exports(&mut self, renderer: &mut GlesRenderer) {
        if !self.toplevel_export_state.has_pending() {
            return;
        }

        let _span = tracy_client::span!("Arbor::render_pending_toplevel_exports");

        let layout = &self.layout;
        self.toplevel_export_state
            .retain_windows(|id| layout.windows().any(|(_, m)| m.id().get() == id));

        for export in self.toplevel_export_state.take_pending() {
            if let Some(export) = self.render_toplevel_export(renderer, export) {
                self.toplevel_export_state.push(export);
            }
        }
    }

    /// Renders one window capture. Returns it back if it has to wait for damage. Dropping the
    /// export (window gone, render error) fails the frame.
    pub fn render_toplevel_export(
        &mut self,
        renderer: &mut GlesRenderer,
        export: ToplevelExport,
    ) -> Option<ToplevelExport> {
        if !export.is_alive() {
            return None;
        }

        let (_, mapped) = self
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id().get() == export.window)?;

        let scale = Scale::from(export.scale);
        let mut elements = Vec::new();
        mapped.render(
            RenderCtx {
                renderer,
                target: RenderTarget::Screencast,
                xray: None,
            },
            Point::from((0., 0.)),
            scale,
            1.,
            XrayPos::default(),
            &mut |elem| elements.push(elem),
        );

        let tracker = self.toplevel_export_state.tracker_for(&export);
        let damage = match tracker.damage_output(1, &elements) {
            Ok((damage, _states)) => damage.cloned(),
            Err(err) => {
                warn!("toplevel export: error computing damage: {err:?}");
                return None;
            }
        };

        let damage = match damage {
            Some(damage) if !damage.is_empty() => damage,
            _ if export.ignore_damage => vec![Rectangle::from_size(export.size)],
            _ => return Some(export),
        };
        if !export.ignore_damage {
            export.damage(damage.into_iter());
        }

        let size = export.size;
        let res = match export.buffer().clone() {
            ExportBuffer::Dmabuf(dmabuf) => {
                render_elements_to_dmabuf(renderer, dmabuf, size, scale, elements.iter().rev())
                    .map(Some)
            }
            ExportBuffer::Shm(buffer) => {
                render_to_vec(
                    renderer,
                    size,
                    scale,
                    Transform::Normal,
                    EXPORT_FOURCC,
                    elements.iter().rev(),
                )
                .and_then(|pixels| {
                    shm::with_buffer_contents_mut(&buffer, |ptr, len, data| {
                        let n = (data.stride * data.height) as usize;
                        anyhow::ensure!(pixels.len() >= n && len >= n, "shm buffer too small");
                        // SAFETY: the pool is mapped for at least `len` bytes at `ptr`.
                        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), ptr, n) };
                        Ok(())
                    })
                    .map_err(|err| anyhow::anyhow!("{err:?}"))?
                })
                .map(|()| None)
            }
        };

        match res {
            Ok(sync) => export.submit_after_sync(sync, &self.event_loop),
            Err(err) => {
                warn!("toplevel export: error rendering: {err:?}");
                drop(export);
            }
        }
        None
    }
}
