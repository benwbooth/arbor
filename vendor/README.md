# Vendored crates

- `smithay/`, `smithay-drm-extras/`: [Smithay](https://github.com/Smithay/smithay) at
  `ff5fa7df392cecfba049ffed55cdaa4e98a8e7ef` (MIT, `LICENSE.txt`). Only the library sources are
  kept (no anvil, examples, benches); the manifests lost their `[workspace]`, examples, benches
  and dev-dependencies. arbor's changes are marked `// arbor:` in the sources and listed here.

  To update: copy `src/` (and `smithay-drm-extras/src/`) from a newer Smithay over these,
  re-apply the changes below, and check `cargo build` and `cargo test`.

Changes:
- `SelectionHandler::selection_without_focus_allowed` (default false): `wl_data_device` and
  primary-selection `set_selection` from such a client is accepted without keyboard focus. arbor
  allows it for apps its supervisor's Wayland proxy is rebuilding after a restart, so the app
  that owned the clipboard keeps owning it.
- ext-session-lock: the outputs a lock has surfaces for are kept per lock object
  (`SessionLockState::locked_outputs`), not in one list on `SessionLockManagerState` that only
  `unlock_and_destroy` cleared. Upstream, once a client's lock was refused, any later lock from
  that client posted `duplicated_output` for the same output and killed the client.
- ext-session-lock: `unlock_and_destroy` on a lock that was never confirmed posts
  `invalid_unlock` and stops. Upstream it then unlocked anyway, so a client whose lock was
  refused could unlock the session another client had locked.
- Surface damage at a fractional scale grows by one physical pixel on every side
  (`WaylandSurfaceRenderElement::damage_since`): clients such as Qt round their buffer damage a
  pixel short there, and the missed row or column kept the previous frame's pixels, so moving
  text left a blurry ghost on screen only.
