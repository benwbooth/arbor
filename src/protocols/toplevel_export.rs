//! Window capture for shells: `hyprland_toplevel_export_manager_v1` (version 2).
//!
//! Quickshell's `ScreencopyView { captureSource: <Toplevel> }` (hearth's dock previews,
//! overview and switcher thumbnails) captures windows through this protocol, addressing them by
//! their `zwlr_foreign_toplevel_handle_v1`. `capture_toplevel` (the plain `uint` handle) takes
//! the arbor window id as seen on IPC (`arbor msg windows`).
//!
//! Frames contain the window geometry (no server-side title bar, no compositor rounding; popups
//! clipped to the window). Copies without `ignore_damage` wait until the window has damage since
//! the previous copy made through the same manager object; they're checked on every output
//! redraw, so windows on hidden workspaces, minimized windows and hidden tabs work too (they show
//! their last content until they commit again).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Fourcc};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm::Format;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Physical, Rectangle, Size, Transform};
use smithay::wayland::{dmabuf, shm};

use super::raw::hyprland_toplevel_export::v1::server::{
    hyprland_toplevel_export_frame_v1, hyprland_toplevel_export_manager_v1,
};
use crate::utils::get_monotonic_time;
use hyprland_toplevel_export_frame_v1::HyprlandToplevelExportFrameV1;
use hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1;

const VERSION: u32 = 2;

/// Pixel format offered for both shm and dmabuf buffers (alpha kept: CSD corners stay round).
pub const EXPORT_FOURCC: Fourcc = Fourcc::Argb8888;
const EXPORT_SHM_FORMAT: Format = Format::Argb8888;

/// The window a capture refers to, resolved by the handler.
#[derive(Debug, Clone, Copy)]
pub struct ExportTarget {
    /// arbor window id (`MappedId`).
    pub window: u64,
    /// Buffer size: the window geometry at `scale`.
    pub size: Size<i32, Physical>,
    pub scale: f64,
}

#[derive(Default)]
pub struct ToplevelExportManagerState {
    /// Copies waiting for damage.
    pending: Vec<ToplevelExport>,
    /// Damage trackers per (manager object, window): damage is reported since the previous copy
    /// made through the same manager.
    trackers: HashMap<(ObjectId, u64), OutputDamageTracker>,
}

pub struct ToplevelExportGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

pub trait ToplevelExportHandler {
    fn toplevel_export_state(&mut self) -> &mut ToplevelExportManagerState;
    /// Resolves a wlr foreign toplevel handle to the window it stands for.
    fn export_target_for_handle(
        &mut self,
        handle: &ZwlrForeignToplevelHandleV1,
    ) -> Option<ExportTarget>;
    /// Resolves an arbor window id.
    fn export_target_for_id(&mut self, id: u64) -> Option<ExportTarget>;
    /// A client asked to copy a frame. The handler must synchronously either finish it (render,
    /// or drop it to fail it) or queue it with [`ToplevelExportManagerState::push`].
    fn export_frame(&mut self, export: ToplevelExport);
}

impl ToplevelExportManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<HyprlandToplevelExportManagerV1, ToplevelExportGlobalData>,
        D: Dispatch<HyprlandToplevelExportManagerV1, ()>,
        D: Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>,
        D: ToplevelExportHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ToplevelExportGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, HyprlandToplevelExportManagerV1, _>(VERSION, global_data);
        Self::default()
    }

    pub fn push(&mut self, export: ToplevelExport) {
        self.pending.push(export);
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn take_pending(&mut self) -> Vec<ToplevelExport> {
        std::mem::take(&mut self.pending)
    }

    /// The damage tracker for this copy, recreated (full damage) when the size or scale changed.
    pub fn tracker_for(&mut self, export: &ToplevelExport) -> &mut OutputDamageTracker {
        let key = (export.manager.clone(), export.window);
        let size = export.size;
        let scale = export.scale;
        let tracker = self
            .trackers
            .entry(key)
            .or_insert_with(|| OutputDamageTracker::new(size, scale, Transform::Normal));
        let matches = matches!(
            tracker.mode(),
            smithay::output::OutputModeSource::Static { size: s, scale: sc, .. }
                if *s == size && sc.x == scale
        );
        if !matches {
            *tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        }
        tracker
    }

    /// Forgets damage trackers of windows that are gone and copies of destroyed frames.
    pub fn retain_windows(&mut self, alive: impl Fn(u64) -> bool) {
        self.trackers.retain(|(_, window), _| alive(*window));
        self.pending
            .retain(|export| export.frame.is_alive() && alive(export.window));
    }

    fn remove_manager(&mut self, manager: &ObjectId) {
        self.trackers.retain(|(m, _), _| m != manager);
    }
}

impl<D> GlobalDispatch<HyprlandToplevelExportManagerV1, ToplevelExportGlobalData, D>
    for ToplevelExportManagerState
where
    D: GlobalDispatch<HyprlandToplevelExportManagerV1, ToplevelExportGlobalData>,
    D: Dispatch<HyprlandToplevelExportManagerV1, ()>,
    D: Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>,
    D: ToplevelExportHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<HyprlandToplevelExportManagerV1>,
        _global_data: &ToplevelExportGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }

    fn can_view(client: Client, global_data: &ToplevelExportGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<HyprlandToplevelExportManagerV1, (), D> for ToplevelExportManagerState
where
    D: Dispatch<HyprlandToplevelExportManagerV1, ()>,
    D: Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>,
    D: ToplevelExportHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &HyprlandToplevelExportManagerV1,
        request: hyprland_toplevel_export_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let (frame, overlay_cursor, target) = match request {
            hyprland_toplevel_export_manager_v1::Request::CaptureToplevel {
                frame,
                overlay_cursor,
                handle,
            } => (frame, overlay_cursor, state.export_target_for_id(u64::from(handle))),
            hyprland_toplevel_export_manager_v1::Request::CaptureToplevelWithWlrToplevelHandle {
                frame,
                overlay_cursor,
                handle,
            } => (frame, overlay_cursor, state.export_target_for_handle(&handle)),
            hyprland_toplevel_export_manager_v1::Request::Destroy => return,
        };

        let Some(target) = target.filter(|t| t.size.w > 0 && t.size.h > 0) else {
            trace!("toplevel export: no such window (or it has no size yet)");
            let frame = data_init.init(frame, ToplevelExportFrameData::failed());
            frame.failed();
            return;
        };

        let frame = data_init.init(
            frame,
            ToplevelExportFrameData {
                inner: Mutex::new(FrameInner::Pending {
                    manager: manager.id(),
                    target,
                    overlay_cursor: overlay_cursor != 0,
                    copied: false,
                }),
            },
        );

        let (w, h) = (target.size.w as u32, target.size.h as u32);
        frame.buffer(EXPORT_SHM_FORMAT, w, h, w * 4);
        frame.linux_dmabuf(EXPORT_FOURCC as u32, w, h);
        frame.buffer_done();
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        manager: &HyprlandToplevelExportManagerV1,
        _data: &(),
    ) {
        state.toplevel_export_state().remove_manager(&manager.id());
    }
}

pub struct ToplevelExportFrameData {
    inner: Mutex<FrameInner>,
}

impl ToplevelExportFrameData {
    fn failed() -> Self {
        Self {
            inner: Mutex::new(FrameInner::Failed),
        }
    }
}

enum FrameInner {
    Failed,
    Pending {
        manager: ObjectId,
        target: ExportTarget,
        overlay_cursor: bool,
        copied: bool,
    },
}

impl<D> Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData, D>
    for ToplevelExportManagerState
where
    D: Dispatch<HyprlandToplevelExportFrameV1, ToplevelExportFrameData>,
    D: ToplevelExportHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        frame: &HyprlandToplevelExportFrameV1,
        request: hyprland_toplevel_export_frame_v1::Request,
        data: &ToplevelExportFrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let (buffer, ignore_damage) = match request {
            hyprland_toplevel_export_frame_v1::Request::Copy {
                buffer,
                ignore_damage,
            } => (buffer, ignore_damage != 0),
            hyprland_toplevel_export_frame_v1::Request::Destroy => return,
        };

        let (manager, target, overlay_cursor) = {
            let mut inner = data.inner.lock().unwrap();
            match &mut *inner {
                FrameInner::Failed => {
                    frame.failed();
                    return;
                }
                FrameInner::Pending {
                    manager,
                    target,
                    overlay_cursor,
                    copied,
                } => {
                    if *copied {
                        frame.post_error(
                            hyprland_toplevel_export_frame_v1::Error::AlreadyUsed,
                            "copy was already requested",
                        );
                        return;
                    }
                    *copied = true;
                    (manager.clone(), *target, *overlay_cursor)
                }
            }
        };

        let size = target.size;
        let buffer = if let Ok(dmabuf) = dmabuf::get_dmabuf(&buffer) {
            let format_ok = matches!(dmabuf.format().code, Fourcc::Argb8888 | Fourcc::Xrgb8888);
            if format_ok && dmabuf.width() == size.w as u32 && dmabuf.height() == size.h as u32 {
                ExportBuffer::Dmabuf(dmabuf.clone())
            } else {
                frame.post_error(
                    hyprland_toplevel_export_frame_v1::Error::InvalidBuffer,
                    "invalid dmabuf parameters",
                );
                return;
            }
        } else if shm::with_buffer_contents(&buffer, |_, shm_len, data| {
            matches!(data.format, Format::Argb8888 | Format::Xrgb8888)
                && data.width == size.w
                && data.height == size.h
                && data.stride == size.w * 4
                && shm_len >= data.stride as usize * data.height as usize
        })
        .unwrap_or(false)
        {
            ExportBuffer::Shm(buffer)
        } else {
            frame.post_error(
                hyprland_toplevel_export_frame_v1::Error::InvalidBuffer,
                "invalid buffer",
            );
            return;
        };

        state.export_frame(ToplevelExport {
            frame: frame.clone(),
            manager,
            window: target.window,
            size,
            scale: target.scale,
            overlay_cursor,
            ignore_damage,
            buffer,
            submitted: false,
        });
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        frame: &HyprlandToplevelExportFrameV1,
        _data: &ToplevelExportFrameData,
    ) {
        state
            .toplevel_export_state()
            .pending
            .retain(|export| export.frame != *frame);
    }
}

#[derive(Clone)]
pub enum ExportBuffer {
    Dmabuf(Dmabuf),
    Shm(WlBuffer),
}

/// A copy request for one window frame. Dropping it unsubmitted fails the frame.
pub struct ToplevelExport {
    frame: HyprlandToplevelExportFrameV1,
    manager: ObjectId,
    pub window: u64,
    pub size: Size<i32, Physical>,
    pub scale: f64,
    pub overlay_cursor: bool,
    pub ignore_damage: bool,
    buffer: ExportBuffer,
    submitted: bool,
}

impl Drop for ToplevelExport {
    fn drop(&mut self) {
        if !self.submitted && self.frame.is_alive() {
            self.frame.failed();
        }
    }
}

impl ToplevelExport {
    pub fn buffer(&self) -> &ExportBuffer {
        &self.buffer
    }

    pub fn is_alive(&self) -> bool {
        self.frame.is_alive()
    }

    pub fn damage(&self, damage: impl Iterator<Item = Rectangle<i32, Physical>>) {
        for rect in damage {
            let Some(rect) = rect.intersection(Rectangle::from_size(self.size)) else {
                continue;
            };
            self.frame.damage(
                rect.loc.x as u32,
                rect.loc.y as u32,
                rect.size.w as u32,
                rect.size.h as u32,
            );
        }
    }

    fn submit(mut self, timestamp: Duration) {
        self.frame
            .flags(hyprland_toplevel_export_frame_v1::Flags::empty());
        let secs = timestamp.as_secs();
        self.frame.ready(
            (secs >> 32) as u32,
            (secs & 0xFFFF_FFFF) as u32,
            timestamp.subsec_nanos(),
        );
        self.submitted = true;
    }

    pub fn submit_after_sync<T>(
        self,
        sync_point: Option<SyncPoint>,
        event_loop: &LoopHandle<'_, T>,
    ) {
        let timestamp = get_monotonic_time();
        match sync_point.and_then(|s| s.export()) {
            None => self.submit(timestamp),
            Some(sync_fd) => {
                let source = Generic::new(sync_fd, Interest::READ, Mode::OneShot);
                let mut export = Some(self);
                event_loop
                    .insert_source(source, move |_, _, _| {
                        export.take().unwrap().submit(timestamp);
                        Ok(PostAction::Remove)
                    })
                    .unwrap();
            }
        }
    }
}

#[allow(missing_docs)]
#[macro_export]
macro_rules! delegate_toplevel_export {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::hyprland_toplevel_export::v1::server::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1: $crate::protocols::toplevel_export::ToplevelExportGlobalData
        ] => $crate::protocols::toplevel_export::ToplevelExportManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::hyprland_toplevel_export::v1::server::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1: ()
        ] => $crate::protocols::toplevel_export::ToplevelExportManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::hyprland_toplevel_export::v1::server::hyprland_toplevel_export_frame_v1::HyprlandToplevelExportFrameV1: $crate::protocols::toplevel_export::ToplevelExportFrameData
        ] => $crate::protocols::toplevel_export::ToplevelExportManagerState);
    };
}
