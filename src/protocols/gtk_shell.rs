//! GTK's private shell protocol (`gtk_shell1`), for global menus.
//!
//! GTK 3 and 4 apps bind `gtk_shell1` when the compositor offers it and tell it, per toplevel
//! `wl_surface`, where their `GtkApplication` exports its menus on the session bus
//! (`gtk_surface1.set_dbus_properties`: unique bus name, `org.gtk.Menus` paths for the menu bar
//! and app menu, `org.gtk.Actions` paths for the `app.` and `win.` actions). This is how Unity
//! and GNOME Shell found GTK menus; appmenu-gtk-module uses it too for plain `GtkMenuBar`s. We
//! store the properties in the surface's data map (like `kde_appmenu`), and IPC shows them as
//! `gtk_menu` on windows for the shell's menu bar.
//!
//! The `global_menu_bar` capability makes GTK hide the in-window menu bar of
//! `GtkApplicationWindow`s (the `gtk-shell-shows-menubar` setting), `global_app_menu` stops
//! GTK 3 from folding a GNOME 3 style app menu into that menu bar (the shell shows it under
//! the app's name instead). We only advertise it while
//! something draws global menus, i.e. while `com.canonical.AppMenu.Registrar` has an owner on
//! the session bus, which is also when Qt apps hide theirs. GTK 3 and 4 pick changes up live.
//!
//! Everything else in the protocol is accepted and ignored: we never send `configure`, and we
//! offer version 4 so that GTK keeps handling titlebar gestures itself (version 5).

use std::sync::Mutex;

use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::wayland::compositor::with_states;

use super::raw::gtk_shell::v1::server::gtk_shell1::{self, GtkShell1};
use super::raw::gtk_shell::v1::server::gtk_surface1::{self, GtkSurface1};

const VERSION: u32 = 4;

/// Where a GTK window's menus live on the session bus.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GtkMenuAddress {
    pub application_id: String,
    pub unique_bus_name: String,
    /// `org.gtk.Menus` object of the menu bar.
    pub menubar_path: String,
    /// `org.gtk.Menus` object of the (GNOME 3 style) app menu.
    pub app_menu_path: String,
    /// `org.gtk.Actions` object with the `win.` actions.
    pub window_object_path: String,
    /// `org.gtk.Actions` object with the `app.` actions.
    pub application_object_path: String,
}

#[derive(Default)]
struct GtkMenuSurfaceData(Mutex<Option<GtkMenuAddress>>);

/// Returns the GTK menu address the client set for this surface, if any.
pub fn gtk_menu_address(surface: &WlSurface) -> Option<GtkMenuAddress> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<GtkMenuSurfaceData>()
            .and_then(|data| data.0.lock().unwrap().clone())
    })
}

fn set_gtk_menu_address(surface: &WlSurface, address: Option<GtkMenuAddress>) {
    with_states(surface, |states| {
        let data = states
            .data_map
            .get_or_insert_threadsafe(GtkMenuSurfaceData::default);
        *data.0.lock().unwrap() = address;
    });
}

pub struct GtkShellState {
    shells: Vec<GtkShell1>,
    global_menu_bar: bool,
}

pub struct GtkShellGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

pub struct GtkSurfaceData {
    surface: Weak<WlSurface>,
}

pub trait GtkShellHandler {
    fn gtk_shell_state(&mut self) -> &mut GtkShellState;
}

impl GtkShellState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<GtkShell1, GtkShellGlobalData>,
        D: Dispatch<GtkShell1, ()>,
        D: Dispatch<GtkSurface1, GtkSurfaceData>,
        D: GtkShellHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = GtkShellGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, GtkShell1, _>(VERSION, global_data);

        Self {
            shells: Vec::new(),
            global_menu_bar: false,
        }
    }

    fn capabilities(&self) -> u32 {
        if self.global_menu_bar {
            gtk_shell1::Capability::GlobalMenuBar as u32
                | gtk_shell1::Capability::GlobalAppMenu as u32
        } else {
            0
        }
    }

    /// Whether a global menu bar is on screen (GTK then hides in-window menu bars).
    pub fn set_global_menu_bar(&mut self, on: bool) {
        if self.global_menu_bar == on {
            return;
        }
        self.global_menu_bar = on;
        self.shells.retain(|s| s.is_alive());
        let caps = self.capabilities();
        for shell in &self.shells {
            shell.capabilities(caps);
        }
    }
}

impl<D> GlobalDispatch<GtkShell1, GtkShellGlobalData, D> for GtkShellState
where
    D: GlobalDispatch<GtkShell1, GtkShellGlobalData>,
    D: Dispatch<GtkShell1, ()>,
    D: Dispatch<GtkSurface1, GtkSurfaceData>,
    D: GtkShellHandler,
    D: 'static,
{
    fn bind(
        state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        shell: New<GtkShell1>,
        _global_data: &GtkShellGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let shell = data_init.init(shell, ());
        let gtk = state.gtk_shell_state();
        shell.capabilities(gtk.capabilities());
        gtk.shells.retain(|s| s.is_alive());
        gtk.shells.push(shell);
    }

    fn can_view(client: Client, global_data: &GtkShellGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<GtkShell1, (), D> for GtkShellState
where
    D: Dispatch<GtkShell1, ()>,
    D: Dispatch<GtkSurface1, GtkSurfaceData>,
    D: GtkShellHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &GtkShell1,
        request: <GtkShell1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            gtk_shell1::Request::GetGtkSurface {
                gtk_surface,
                surface,
            } => {
                data_init.init(
                    gtk_surface,
                    GtkSurfaceData {
                        surface: surface.downgrade(),
                    },
                );
            }
            // Startup notification and the bell go through xdg-activation and nothing.
            gtk_shell1::Request::SetStartupId { .. }
            | gtk_shell1::Request::SystemBell { .. }
            | gtk_shell1::Request::NotifyLaunch { .. } => (),
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, resource: &GtkShell1, _data: &()) {
        state.gtk_shell_state().shells.retain(|s| s != resource);
    }
}

impl<D> Dispatch<GtkSurface1, GtkSurfaceData, D> for GtkShellState
where
    D: Dispatch<GtkSurface1, GtkSurfaceData>,
    D: GtkShellHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &GtkSurface1,
        request: <GtkSurface1 as Resource>::Request,
        data: &GtkSurfaceData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let Ok(surface) = data.surface.upgrade() else {
            return;
        };

        match request {
            gtk_surface1::Request::SetDbusProperties {
                application_id,
                app_menu_path,
                menubar_path,
                window_object_path,
                application_object_path,
                unique_bus_name,
            } => {
                let address = GtkMenuAddress {
                    application_id: application_id.unwrap_or_default(),
                    unique_bus_name: unique_bus_name.unwrap_or_default(),
                    menubar_path: menubar_path.unwrap_or_default(),
                    app_menu_path: app_menu_path.unwrap_or_default(),
                    window_object_path: window_object_path.unwrap_or_default(),
                    application_object_path: application_object_path.unwrap_or_default(),
                };
                trace!("gtk menus for {surface:?}: {address:?}");
                let useful = !address.unique_bus_name.is_empty()
                    && !(address.menubar_path.is_empty() && address.app_menu_path.is_empty());
                set_gtk_menu_address(&surface, useful.then_some(address));
            }
            gtk_surface1::Request::Release => set_gtk_menu_address(&surface, None),
            // Modality, present and focus requests: xdg-shell and xdg-activation cover them.
            _ => (),
        }
    }
}

#[macro_export]
macro_rules! delegate_gtk_shell {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::gtk_shell::v1::server::gtk_shell1::GtkShell1: $crate::protocols::gtk_shell::GtkShellGlobalData
        ] => $crate::protocols::gtk_shell::GtkShellState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::gtk_shell::v1::server::gtk_shell1::GtkShell1: ()
        ] => $crate::protocols::gtk_shell::GtkShellState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::protocols::raw::gtk_shell::v1::server::gtk_surface1::GtkSurface1: $crate::protocols::gtk_shell::GtkSurfaceData
        ] => $crate::protocols::gtk_shell::GtkShellState);
    };
}
