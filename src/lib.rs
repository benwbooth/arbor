#[macro_use]
extern crate tracing;

#[cfg(feature = "dbus")]
pub mod a11y;
pub mod animation;
pub mod arbor;
pub mod backend;
pub mod cli;
pub mod clipboard_persist;
pub mod cursor;
#[cfg(feature = "dbus")]
pub mod dbus;
pub mod frame_clock;
pub mod glass;
pub mod global_shortcuts;
pub mod handlers;
pub mod input;
pub mod ipc;
pub mod kglobalaccel;
pub mod layer;
pub mod layout;
pub mod protocols;
pub mod render_helpers;
pub mod rubber_band;
#[cfg(feature = "xdp-gnome-screencast")]
pub mod screencasting;
pub mod session;
pub mod ui;
pub mod utils;
pub mod window;
pub mod zoom;

#[cfg(test)]
mod tests;
