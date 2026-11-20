//! KDE appmenu protocol (`org_kde_kwin_appmenu_manager`).
//!
//! Clients (KDE/Qt apps with the KDE platform theme) export their menu bar over DBus as
//! `com.canonical.dbusmenu` and use this protocol to tell the compositor where it lives, per
//! `wl_surface`. We store the address in the surface's data map so that it goes away together
//! with the surface; IPC picks it up from there for the window's root surface.

use std::sync::Mutex;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::wayland::compositor::with_states;
use wayland_protocols_plasma::appmenu::server::org_kde_kwin_appmenu::{self, OrgKdeKwinAppmenu};
use wayland_protocols_plasma::appmenu::server::org_kde_kwin_appmenu_manager::{
    self, OrgKdeKwinAppmenuManager,
};

const VERSION: u32 = 2;

/// DBus location of a surface's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMenuAddress {
    pub service_name: String,
    pub object_path: String,
}

/// Per-surface storage, in the surface's data map.
#[derive(Default)]
struct AppMenuSurfaceData(Mutex<Option<AppMenuAddress>>);

/// Returns the appmenu address the client set for this surface, if any.
pub fn app_menu_address(surface: &WlSurface) -> Option<AppMenuAddress> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<AppMenuSurfaceData>()
            .and_then(|data| data.0.lock().unwrap().clone())
    })
}

fn set_app_menu_address(surface: &WlSurface, address: Option<AppMenuAddress>) {
    with_states(surface, |states| {
        let data = states
            .data_map
            .get_or_insert_threadsafe(AppMenuSurfaceData::default);
        *data.0.lock().unwrap() = address;
    });
}

pub struct KdeAppmenuManagerState {}

pub struct KdeAppmenuManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

/// Data of an `org_kde_kwin_appmenu` object: the surface it was created for.
pub struct KdeAppmenuData {
    surface: Weak<WlSurface>,
}

pub trait KdeAppmenuHandler {}

impl KdeAppmenuManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<OrgKdeKwinAppmenuManager, KdeAppmenuManagerGlobalData>,
        D: Dispatch<OrgKdeKwinAppmenuManager, ()>,
        D: Dispatch<OrgKdeKwinAppmenu, KdeAppmenuData>,
        D: KdeAppmenuHandler,
        D: 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = KdeAppmenuManagerGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, OrgKdeKwinAppmenuManager, _>(VERSION, global_data);

        Self {}
    }
}

impl<D> GlobalDispatch<OrgKdeKwinAppmenuManager, KdeAppmenuManagerGlobalData, D>
    for KdeAppmenuManagerState
where
    D: GlobalDispatch<OrgKdeKwinAppmenuManager, KdeAppmenuManagerGlobalData>,
    D: Dispatch<OrgKdeKwinAppmenuManager, ()>,
    D: Dispatch<OrgKdeKwinAppmenu, KdeAppmenuData>,
    D: KdeAppmenuHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        manager: New<OrgKdeKwinAppmenuManager>,
        _manager_state: &KdeAppmenuManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }

    fn can_view(client: Client, global_data: &KdeAppmenuManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<OrgKdeKwinAppmenuManager, (), D> for KdeAppmenuManagerState
where
    D: Dispatch<OrgKdeKwinAppmenuManager, ()>,
    D: Dispatch<OrgKdeKwinAppmenu, KdeAppmenuData>,
    D: KdeAppmenuHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &OrgKdeKwinAppmenuManager,
        request: <OrgKdeKwinAppmenuManager as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            org_kde_kwin_appmenu_manager::Request::Create { id, surface } => {
                data_init.init(
                    id,
                    KdeAppmenuData {
                        surface: surface.downgrade(),
                    },
                );
            }
            org_kde_kwin_appmenu_manager::Request::Release => (),
            _ => unreachable!(),
        }
    }
}

impl<D> Dispatch<OrgKdeKwinAppmenu, KdeAppmenuData, D> for KdeAppmenuManagerState
where
    D: Dispatch<OrgKdeKwinAppmenu, KdeAppmenuData>,
    D: KdeAppmenuHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &OrgKdeKwinAppmenu,
        request: <OrgKdeKwinAppmenu as Resource>::Request,
        data: &KdeAppmenuData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let Ok(surface) = data.surface.upgrade() else {
            return;
        };

        match request {
            org_kde_kwin_appmenu::Request::SetAddress {
                service_name,
                object_path,
            } => {
                trace!("appmenu for {surface:?}: {service_name} {object_path}");
                let address = (!service_name.is_empty() && !object_path.is_empty()).then_some(
                    AppMenuAddress {
                        service_name,
                        object_path,
                    },
                );
                set_app_menu_address(&surface, address);
            }
            // "If not applicable, clients should remove this object."
            org_kde_kwin_appmenu::Request::Release => set_app_menu_address(&surface, None),
            _ => unreachable!(),
        }
    }
}

#[macro_export]
macro_rules! delegate_kde_appmenu {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            wayland_protocols_plasma::appmenu::server::org_kde_kwin_appmenu_manager::OrgKdeKwinAppmenuManager: $crate::protocols::kde_appmenu::KdeAppmenuManagerGlobalData
        ] => $crate::protocols::kde_appmenu::KdeAppmenuManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            wayland_protocols_plasma::appmenu::server::org_kde_kwin_appmenu_manager::OrgKdeKwinAppmenuManager: ()
        ] => $crate::protocols::kde_appmenu::KdeAppmenuManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            wayland_protocols_plasma::appmenu::server::org_kde_kwin_appmenu::OrgKdeKwinAppmenu: $crate::protocols::kde_appmenu::KdeAppmenuData
        ] => $crate::protocols::kde_appmenu::KdeAppmenuManagerState);
    };
}
