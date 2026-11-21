//! arbor: window capture through `hyprland_toplevel_export_manager_v1`, the way Quickshell's
//! `ScreencopyView { captureSource: <Toplevel> }` does it.

use std::os::fd::AsFd as _;
use std::sync::{Arc, Mutex};

use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};

use self::proto::hyprland_toplevel_export_frame_v1::{self, HyprlandToplevelExportFrameV1};
use self::proto::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1;
use super::client::{ClientId, State};
use super::*;

#[allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
#[allow(non_upper_case_globals, non_snake_case, unused_imports)]
#[allow(missing_docs, clippy::all)]
mod proto {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("resources/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("resources/hyprland-toplevel-export-v1.xml");
}

#[derive(Default)]
struct Globals {
    export: Option<HyprlandToplevelExportManagerV1>,
    shm: Option<WlShm>,
    toplevels: Option<ZwlrForeignToplevelManagerV1>,
}

#[derive(Default)]
struct Handles {
    handles: Vec<(ZwlrForeignToplevelHandleV1, Option<String>)>,
}

#[derive(Default, Debug)]
struct FrameEvents {
    shm: Option<(u32, u32, u32, u32)>,
    dmabuf: bool,
    buffer_done: bool,
    damage: Vec<(u32, u32, u32, u32)>,
    ready: bool,
    failed: bool,
}

type G = Arc<Mutex<Globals>>;
type H = Arc<Mutex<Handles>>;
type F = Arc<Mutex<FrameEvents>>;

impl Dispatch<WlRegistry, G> for State {
    fn event(
        _state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        data: &G,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            let mut g = data.lock().unwrap();
            if interface == HyprlandToplevelExportManagerV1::interface().name {
                g.export = Some(registry.bind(name, version.min(2), qh, ()));
            } else if interface == WlShm::interface().name {
                g.shm = Some(registry.bind(name, 1, qh, ()));
            } else if interface == ZwlrForeignToplevelManagerV1::interface().name {
                g.toplevels = Some(registry.bind(name, version.min(3), qh, H::default()));
            }
        }
    }
}

impl Dispatch<HyprlandToplevelExportManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelExportManagerV1,
        _: <HyprlandToplevelExportManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HyprlandToplevelExportFrameV1, F> for State {
    fn event(
        _: &mut Self,
        _: &HyprlandToplevelExportFrameV1,
        event: hyprland_toplevel_export_frame_v1::Event,
        data: &F,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use hyprland_toplevel_export_frame_v1::Event;
        let mut f = data.lock().unwrap();
        match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => f.shm = Some((u32::from(format), width, height, stride)),
            Event::LinuxDmabuf { .. } => f.dmabuf = true,
            Event::BufferDone => f.buffer_done = true,
            Event::Damage {
                x,
                y,
                width,
                height,
            } => f.damage.push((x, y, width, height)),
            Event::Ready { .. } => f.ready = true,
            Event::Failed => f.failed = true,
            Event::Flags { .. } => (),
        }
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: <WlShmPool as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, u8> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &u8,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, H> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        data: &H,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            data.lock().unwrap().handles.push((toplevel, None));
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelHandleV1,
        _: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn color_window(
    f: &mut Fixture,
    id: ClientId,
    (w, h): (u16, u16),
    rgb: (u32, u32, u32),
) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    let buffer = window
        .spbm
        .create_u32_rgba_buffer(rgb.0, rgb.1, rgb.2, u32::MAX, &window.qh, ());
    window.surface.attach(Some(&buffer), 0, 0);
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    surface
}

struct ShmTarget {
    buffer: WlBuffer,
    memfd: std::fs::File,
    len: usize,
}

impl ShmTarget {
    fn new(f: &mut Fixture, id: ClientId, shm: &WlShm, w: u32, h: u32, format: u32) -> Self {
        let len = (w * h * 4) as usize;
        let fd = smithay::reexports::rustix::fs::memfd_create(
            "export",
            smithay::reexports::rustix::fs::MemfdFlags::CLOEXEC,
        )
        .unwrap();
        let memfd = std::fs::File::from(fd);
        memfd.set_len(len as u64).unwrap();
        let client = f.client(id);
        let pool = shm.create_pool(memfd.as_fd(), len as i32, &client.qh, ());
        let format = wl_shm::Format::try_from(format).unwrap();
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            format,
            &client.qh,
            0u8,
        );
        pool.destroy();
        Self { buffer, memfd, len }
    }

    /// First pixel as stored (Argb8888 little endian: B, G, R, A).
    fn pixel(&self, i: usize) -> [u8; 4] {
        use std::os::unix::fs::FileExt as _;
        let mut px = [0u8; 4];
        assert!(i * 4 + 4 <= self.len);
        self.memfd.read_exact_at(&mut px, (i * 4) as u64).unwrap();
        px
    }
}

fn capture(
    f: &mut Fixture,
    id: ClientId,
    g: &G,
    handle: Option<&ZwlrForeignToplevelHandleV1>,
    window_id: u32,
) -> (HyprlandToplevelExportFrameV1, F) {
    let events = F::default();
    let manager = g.lock().unwrap().export.clone().unwrap();
    let client = f.client(id);
    let frame = match handle {
        Some(handle) => {
            manager.capture_toplevel_with_wlr_toplevel_handle(0, handle, &client.qh, events.clone())
        }
        None => manager.capture_toplevel(0, window_id, &client.qh, events.clone()),
    };
    f.roundtrip(id);
    (frame, events)
}

/// Captures a docked-or-floating window through its wlr foreign toplevel handle, then waits for
/// damage on the second copy. Needs EGL (surfaceless Mesa).
#[test]
#[ignore = "needs a GPU/EGL; run with --ignored"]
fn capture_window_by_handle_and_damage() {
    let mut config = arbor_config::Config::default();
    config.hotkey_overlay.skip_at_startup = true;
    let mut f = Fixture::with_config(config);
    f.arbor_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let g = G::default();
    {
        let client = f.client(id);
        client.display.get_registry(&client.qh, g.clone());
    }
    f.double_roundtrip(id);
    assert!(g.lock().unwrap().export.is_some(), "export global missing");

    let surface = color_window(&mut f, id, (200, 100), (u32::MAX, 0, 0));
    f.double_roundtrip(id);

    let handles = g
        .lock()
        .unwrap()
        .toplevels
        .as_ref()
        .unwrap()
        .data::<H>()
        .unwrap()
        .clone();
    let handle = handles.lock().unwrap().handles[0].0.clone();

    // First frame: ignore_damage, like Quickshell's first copy.
    let (frame, events) = capture(&mut f, id, &g, Some(&handle), 0);
    let (format, w, h, stride) = {
        let e = events.lock().unwrap();
        assert!(e.buffer_done && e.dmabuf, "{e:?}");
        e.shm.unwrap()
    };
    assert_eq!((w, h, stride), (200, 100, 800));
    let shm = g.lock().unwrap().shm.clone().unwrap();
    let target = ShmTarget::new(&mut f, id, &shm, w, h, format);
    frame.copy(&target.buffer, 1);
    f.double_roundtrip(id);
    assert!(events.lock().unwrap().ready, "{:?}", events.lock().unwrap());
    // Opaque red, Argb8888 little endian.
    assert_eq!(target.pixel(0), [0, 0, 255, 255]);
    assert_eq!(target.pixel((w * h - 1) as usize), [0, 0, 255, 255]);
    frame.destroy();

    // Second frame waits for damage.
    let (frame, events) = capture(&mut f, id, &g, Some(&handle), 0);
    frame.copy(&target.buffer, 0);
    f.double_roundtrip(id);
    f.double_roundtrip(id);
    assert!(!events.lock().unwrap().ready, "copied without damage");

    // New content: green.
    {
        let window = f.client(id).window(&surface);
        let buffer = window
            .spbm
            .create_u32_rgba_buffer(0, u32::MAX, 0, u32::MAX, &window.qh, ());
        window.surface.attach(Some(&buffer), 0, 0);
        window.surface.damage(0, 0, 200, 100);
        window.commit();
    }
    f.double_roundtrip(id);
    f.double_roundtrip(id);
    {
        let e = events.lock().unwrap();
        assert!(e.ready, "{e:?}");
        assert!(!e.damage.is_empty());
    }
    assert_eq!(target.pixel(0), [0, 255, 0, 255]);
    frame.destroy();

    // Minimized windows still capture (last content).
    f.arbor().layout.minimize_window(None);
    f.double_roundtrip(id);
    let (frame, events) = capture(&mut f, id, &g, Some(&handle), 0);
    frame.copy(&target.buffer, 1);
    f.double_roundtrip(id);
    assert!(events.lock().unwrap().ready);
    assert_eq!(target.pixel(0), [0, 255, 0, 255]);
}

/// `capture_toplevel` with an arbor window id, and failure for unknown windows. Works without a
/// renderer for the failure path.
#[test]
fn capture_unknown_window_fails() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let g = G::default();
    {
        let client = f.client(id);
        client.display.get_registry(&client.qh, g.clone());
    }
    f.double_roundtrip(id);

    let (_frame, events) = capture(&mut f, id, &g, None, 12345);
    let e = events.lock().unwrap();
    assert!(e.failed && !e.buffer_done, "{e:?}");
}

/// Buffer parameters for a window addressed by its IPC id.
#[test]
fn capture_by_window_id_sends_buffer_info() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let g = G::default();
    {
        let client = f.client(id);
        client.display.get_registry(&client.qh, g.clone());
    }
    f.double_roundtrip(id);
    let _surface = color_window(&mut f, id, (300, 200), (0, 0, u32::MAX));

    let window_id = f
        .arbor()
        .layout
        .windows()
        .map(|(_, m)| m.id().get())
        .next()
        .unwrap();
    let (frame, events) = capture(&mut f, id, &g, None, window_id as u32);
    {
        let e = events.lock().unwrap();
        assert!(e.buffer_done && !e.failed, "{e:?}");
        assert_eq!(e.shm.unwrap().1, 300);
        assert_eq!(e.shm.unwrap().2, 200);
    }

    // Without a renderer the copy fails instead of hanging.
    let shm = g.lock().unwrap().shm.clone().unwrap();
    let (format, w, h, _) = events.lock().unwrap().shm.unwrap();
    let target = ShmTarget::new(&mut f, id, &shm, w, h, format);
    frame.copy(&target.buffer, 1);
    f.double_roundtrip(id);
    assert!(events.lock().unwrap().failed);
}
