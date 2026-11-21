//! Message signatures of every protocol arbor offers, for decoding what goes through the proxy.
//!
//! The supervisor is the same binary as the compositor, so the proxy uses the same generated
//! protocol descriptions. `tests::every_global_is_known` checks this list against the globals a
//! client actually sees.

use std::collections::HashMap;
use std::sync::OnceLock;

use smithay::reexports::wayland_server::{protocol as core, Resource};
use smithay::reexports::{
    wayland_protocols as wp, wayland_protocols_misc as misc, wayland_protocols_wlr as wlr,
};
use wayland_backend::protocol::Interface;

fn iface<R: Resource>() -> &'static Interface {
    R::interface()
}

/// The globals; everything else is reached through their requests and events.
fn roots() -> Vec<&'static Interface> {
    vec![
        &core::__interfaces::WL_DISPLAY_INTERFACE,
        iface::<core::wl_compositor::WlCompositor>(),
        iface::<core::wl_subcompositor::WlSubcompositor>(),
        iface::<core::wl_shm::WlShm>(),
        iface::<core::wl_seat::WlSeat>(),
        iface::<core::wl_output::WlOutput>(),
        iface::<core::wl_data_device_manager::WlDataDeviceManager>(),
        iface::<wp::xdg::shell::server::xdg_wm_base::XdgWmBase>(),
        iface::<wp::xdg::decoration::zv1::server::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>(),
        iface::<wp::xdg::activation::v1::server::xdg_activation_v1::XdgActivationV1>(),
        iface::<wp::xdg::foreign::zv2::server::zxdg_exporter_v2::ZxdgExporterV2>(),
        iface::<wp::xdg::foreign::zv2::server::zxdg_importer_v2::ZxdgImporterV2>(),
        iface::<wp::xdg::xdg_output::zv1::server::zxdg_output_manager_v1::ZxdgOutputManagerV1>(),
        iface::<wp::wp::viewporter::server::wp_viewporter::WpViewporter>(),
        iface::<wp::wp::fractional_scale::v1::server::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>(),
        iface::<wp::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>(),
        iface::<wp::wp::presentation_time::server::wp_presentation::WpPresentation>(),
        iface::<wp::wp::cursor_shape::v1::server::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>(),
        iface::<wp::wp::single_pixel_buffer::v1::server::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1>(),
        iface::<wp::wp::pointer_constraints::zv1::server::zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>(),
        iface::<wp::wp::relative_pointer::zv1::server::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>(),
        iface::<wp::wp::pointer_gestures::zv1::server::zwp_pointer_gestures_v1::ZwpPointerGesturesV1>(),
        iface::<wp::wp::primary_selection::zv1::server::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1>(),
        iface::<wp::wp::idle_inhibit::zv1::server::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1>(),
        iface::<wp::wp::keyboard_shortcuts_inhibit::zv1::server::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1>(),
        iface::<wp::wp::text_input::zv3::server::zwp_text_input_manager_v3::ZwpTextInputManagerV3>(),
        iface::<wp::wp::tablet::zv2::server::zwp_tablet_manager_v2::ZwpTabletManagerV2>(),
        iface::<wp::wp::security_context::v1::server::wp_security_context_manager_v1::WpSecurityContextManagerV1>(),
        iface::<wp::wp::drm_lease::v1::server::wp_drm_lease_device_v1::WpDrmLeaseDeviceV1>(),
        iface::<wp::ext::idle_notify::v1::server::ext_idle_notifier_v1::ExtIdleNotifierV1>(),
        iface::<wp::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1>(),
        iface::<wp::ext::data_control::v1::server::ext_data_control_manager_v1::ExtDataControlManagerV1>(),
        iface::<wp::ext::workspace::v1::server::ext_workspace_manager_v1::ExtWorkspaceManagerV1>(),
        iface::<wp::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>(),
        iface::<wp::ext::background_effect::v1::server::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1>(),
        iface::<misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(),
        iface::<misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>(),
        iface::<misc::server_decoration::server::org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager>(),
        iface::<wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1>(),
        iface::<wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>(),
        iface::<wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>(),
        iface::<wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1>(),
        iface::<wlr::output_management::v1::server::zwlr_output_manager_v1::ZwlrOutputManagerV1>(),
        iface::<wlr::virtual_pointer::v1::server::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>(),
        iface::<wlr::data_control::v1::server::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1>(),
        iface::<wayland_protocols_plasma::appmenu::server::org_kde_kwin_appmenu_manager::OrgKdeKwinAppmenuManager>(),
        iface::<crate::protocols::raw::gtk_shell::v1::server::gtk_shell1::GtkShell1>(),
        iface::<crate::protocols::raw::mutter_x11_interop::v1::server::mutter_x11_interop::MutterX11Interop>(),
        iface::<crate::protocols::raw::hyprland_toplevel_export::v1::server::hyprland_toplevel_export_manager_v1::HyprlandToplevelExportManagerV1>(),
    ]
}

fn collect(iface: &'static Interface, map: &mut HashMap<&'static str, &'static Interface>) {
    if map.insert(iface.name, iface).is_some() {
        return;
    }
    for msg in iface.requests.iter().chain(iface.events) {
        if let Some(child) = msg.child_interface {
            collect(child, map);
        }
        for arg in msg.arg_interfaces {
            collect(arg, map);
        }
    }
}

/// Every interface the proxy can decode, by name.
pub fn by_name(name: &str) -> Option<&'static Interface> {
    static MAP: OnceLock<HashMap<&'static str, &'static Interface>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for root in roots() {
            collect(root, &mut map);
        }
        map
    })
    .get(name)
    .copied()
}

pub fn wl_display() -> &'static Interface {
    &core::__interfaces::WL_DISPLAY_INTERFACE
}
