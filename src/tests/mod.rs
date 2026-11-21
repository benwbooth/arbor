use fixture::Fixture;

mod client;
mod fixture;
mod server;

mod clipboard_persist;
mod expose;
mod floating;
mod glass;
mod kde_appmenu;
#[cfg(feature = "dbus")]
mod kglobalaccel;
mod layer_shell;
#[cfg(feature = "dbus")]
mod portal_global_shortcuts;
#[cfg(feature = "dbus")]
mod portal_input_capture;
#[cfg(feature = "xdp-gnome-screencast")]
mod portal_remote_desktop;
mod remove_output;
mod session;
mod toplevel_export;
mod virtual_keyboard;
mod window_opening;
mod zoom;
