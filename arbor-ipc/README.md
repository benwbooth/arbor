# arbor-ipc

Types and helpers for talking to the arbor Wayland compositor over its IPC
socket (`$ARBOR_SOCKET`). The protocol is niri's, so this crate is a renamed
copy of niri's `niri-ipc` with arbor's actions (camera, minimize, tabs) and
fields added; tools written against niri's IPC keep working with arbor.

It follows arbor's version and is not API-stable in terms of Rust semver.
