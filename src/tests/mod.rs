use fixture::Fixture;

mod client;
mod fixture;
mod server;

mod expose;
mod floating;
mod glass;
mod kde_appmenu;
mod layer_shell;
mod clipboard_persist;
#[cfg(feature = "dbus")]
mod portal_global_shortcuts;
mod remove_output;
mod toplevel_export;
mod window_opening;
