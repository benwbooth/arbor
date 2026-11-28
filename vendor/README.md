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
- Surface damage at a fractional scale grows by ceil(scale) physical pixels on every side
  (`WaylandSurfaceRenderElement::damage_since`): clients such as Qt round their buffer damage a
  pixel short there, and the missed row or column kept the previous frame's pixels, so moving
  text left a blurry ghost on screen only.
- Surface opaque regions at a fractional scale shrink by one physical pixel on every side
  (`WaylandSurfaceRenderElement::opaque_regions`): rounding could claim a half-covered edge row
  as opaque, so what's under it wasn't redrawn and showed stale through it.
- Framebuffer effects (liquid glass) redraw in full when any damage overlaps them, from elements
  above as well as below (`OutputDamageTracker`): redrawing a blur only inside a strip left a
  faint line at the strip's edge.

- `xwayland-satellite/`: [xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite)
  0.8.3, the source nixpkgs builds (MPL-2.0, `LICENSE`), kept whole. Built by itself as
  `packages.xwayland-satellite` (nixpkgs' derivation with this source; same Cargo.lock, so its
  cargo deps are reused) and installed by `nix/module.nix` ahead of any other copy, since arbor
  runs `xwayland-satellite` from PATH. Not part of arbor's cargo workspace.

  To update: replace the directory with a newer release's source, re-apply the changes below,
  and bump nixpkgs to the same version (or give the package its own `cargoHash`).

Changes:
- `_NET_WM_MOVERESIZE` ignores the button field, like KWin. Chromium and Electron title bars
  send button 0 and were dropped ("non left click button (0)"), so their windows couldn't be
  dragged. The compositor still only moves the window while the click's pointer grab is held.
