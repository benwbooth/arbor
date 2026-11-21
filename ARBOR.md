# arbor

A fork of [niri](https://github.com/YaLTeR/niri) (v26.04) that replaces
scrollable tiling with a **nested docking layout under a zoomable camera**.
It's an original window manager built around docking, nesting, and zooming.

Upstream is the `upstream` remote. Rebase `main` onto new niri tags; keep
arbor changes in new files where possible so rebases stay small.

## Names

arbor is fully rebranded for users and other programs; only internal names
(the `niri` package/library crate, `arbor-config`, `arbor-ipc`, `niri::` paths,
source file names, tracing targets, shader uniform names) stay niri's so
rebases stay small.

| What | Name |
| --- | --- |
| Binary | `arbor` (the Cargo package is still `niri`; `[[bin]] name = "arbor"`). `arbor msg`, `arbor validate`, `arbor --session`, `arbor completions`. No `niri` binary. |
| Session | `bin/arbor-session` (starts `arbor.service`), user units `arbor.service` and `arbor-shutdown.target`, `share/wayland-sessions/arbor.desktop` (`DesktopNames=arbor`, so `XDG_CURRENT_DESKTOP=arbor`; `arbor --session` also sets it) |
| Portals | `share/xdg-desktop-portal/arbor-portals.conf`; on NixOS `xdg.portal.config.arbor` |
| Config | `$XDG_CONFIG_HOME/arbor/config.kdl`, override with `--config` or `$ARBOR_CONFIG`; system-wide fallback `/etc/arbor/config.kdl`. The default config is written there on first run. |
| IPC | `$XDG_RUNTIME_DIR/arbor.<wayland-display>.<pid>.sock`, exported as `$ARBOR_SOCKET`. `$NIRI_SOCKET` is exported with the same path so tools with niri IPC support (waybar's niri modules, Quickshell, ...) keep working; `arbor msg` reads `$ARBOR_SOCKET` first, then `$NIRI_SOCKET`. The protocol is niri's. |
| Env | `ARBOR_DISABLE_SYSTEM_MANAGER_NOTIFY=1` skips the systemd/fd ready notification. Spawned apps get transient scopes `app-arbor-*.scope`. |
| Nix | `packages.<system>.arbor` (= `default`, plus `arbor-debug`), `overlays.default` adds `pkgs.arbor`, `nixosModules.arbor` (= `default`) provides `programs.arbor = { enable; package; useNautilus; }` |

D-Bus names that clients depend on (`org.gnome.Mutter.*`,
`org.gnome.Shell.*`) and Wayland protocol names are unchanged.

There is no fallback to niri's config location. To move an old config:

```sh
mkdir -p ~/.config/arbor && mv ~/.config/niri/config.kdl ~/.config/arbor/
```

(plus any files it `include`s). On NixOS replace `programs.niri` with
`programs.arbor` from this flake's module (it does not enable GNOME Keyring;
set the Secret portal yourself) and `xdg.portal.config.niri` with
`xdg.portal.config.arbor`.

## Model

- A workspace holds a **dock tree** plus niri's existing floating space
  (the "Desktop": windows float freely there).
- Dock tree nodes are `Split { axis, children, fractions }` or `Leaf(Tile)`.
  Children of a split never overlap; dividers sit in the gaps.
- **Nothing moves until you dock something.** New windows open floating
  (on the Desktop) unless a window rule says `open-docked`.
- **Docking by drag:** drag a window by its title bar (client-side move
  requests work in niri) over:
  - the edge band of a docked window: it docks beside that window
    (splitting it along that axis, or joining the parent split when the
    axes match);
  - the edge band of the screen: it docks beside everything (new root
    split);
  - the middle of a docked window (center 50%), resting there ~400 ms:
    tab group (for floating and docked drags alike; the insert hint switches
    to the whole target when it arms).
  - empty Desktop area: floats.
  An insert hint shows the target rectangle while dragging.
- **Dividers:** dragging the gap between two siblings resizes just those two.
- **Minimize:** a docked window's siblings take its space; restoring puts it
  back in the same slot (the tree remembers a placeholder).
- **Double-click title / maximize:** frame that window on its own (camera),
  again to return.

## Touchpad

3- and 4-finger swipes work like macOS: vertical opens/closes the overview
(see Overview), horizontal switches workspaces (both interactive).

## Overview

The overview (`toggle-overview` / `open-overview` / `close-overview`, the
top-left hot corner, or a 3/4-finger swipe up) works like macOS Mission
Control and KDE's Overview instead of niri's zoomed-out workspace strip:

- Every shown window of the workspace (docked and floating; the shown tab
  of a tab group) glides from where it is to a slot in a non-overlapping
  layout: rows filling the working area, windows in their rough on-screen
  order, all scaled by one factor (never enlarged), with gaps. Closing
  glides them back. The swipe scrubs the same animation: the windows follow
  the fingers and the release settles open or closed.
- Minimized windows are left out (restore them from the dock/taskbar).
- Under each window: its app icon and title. Hovering a window outlines it
  in the accent colour and shows a close button at its top right; the
  focused window has a subtle outline (arrow keys move the focus,
  Enter/Escape leave the overview on it).
- Along the top: a thumbnail per workspace (window outlines with app
  icons), the shown one outlined in the accent colour, and a "+" thumbnail
  for the empty workspace at the end.
- The wallpaper and bottom layers are dimmed behind it (no blur yet). Panels
  (top/overlay layers) stay on top and clickable.

| Do this | To |
| --- | --- |
| Click a window | Go to it (closes the overview) |
| Click its close button, or middle-click it | Close the window |
| Drag a window onto a workspace thumbnail | Move it to that workspace ("+" makes a new one) |
| Click a workspace thumbnail | Show that workspace's windows |
| Click empty space, Escape, or the overview key again | Leave the overview |

IPC (`OverviewState`, `OverviewOpenedOrClosed`) and the action names are
niri's, so shells and binds keep working. Workspaces no longer zoom out
(`compute_overview_zoom` is 1); the `overview { zoom }` setting has no
effect. Touch: tapping a window or empty space works; dragging by touch
still uses niri's interactive move (full size).

## Camera (milestone 2)

The dock tree is laid out on a *canvas*: the working area, grown where
leaves would get smaller than the minimum useful size (`min-leaf-width` x
`min-leaf-height`; children below their minimum take room from their
siblings first, so the canvas only grows when everything can't fit). A
camera (offset + zoom, per workspace) maps canvas to screen:
`view = (canvas - offset) * zoom`. Docked windows render through it
(`RescaleRenderElement` when zoomed, as the overview does); the floating
Desktop does not move.

- Modes: *follow* (actual size, auto-pans so the focused window is fully
  shown — the default; identical to milestone 1 while the canvas is the
  screen), *framed* (fits the node at some depth on the path to the
  focused window), *show-all*, *free* (pinch).
- Actions (binds and IPC): `camera-zoom-out` (one level up: the parent of
  the focused node, skipping levels that would only pan; ultimately
  everything), `camera-zoom-in` (one level toward the focused window, then
  actual size), `camera-show-all`, `camera-frame-window`,
  `camera-pan-left/right/up/down` (focus the neighbour, camera follows),
  `camera-back` / `camera-forward` (history, restores focus too). Moves
  use the `horizontal-view-movement` animation.
- Touchpad pinch zooms around the pointer, following the fingers: with Mod
  anywhere; without Mod over empty space or over the zoomed-out dock tree
  (pinches over windows at actual size go to the window). Released close
  to actual size (zoom >= 0.85) it settles back to following, animated.
- While zoomed, docked windows take no input: a click activates and starts
  a move (drag still docks), a double-click frames the window. Dividers
  still drag.
- While zoomed, a docked window being dragged and closing windows are
  drawn at the camera's zoom; framing never shows past the canvas edge
  (where the canvas is larger than the view).
- Leaves rendered below a size threshold become an app-icon tile; a whole
  subtree below threshold becomes one tile showing its split lines.

```kdl
layout {
    camera {
        min-leaf-width 480
        min-leaf-height 320
        icon-threshold 160
    }
}
```

## Milestones

1. DockSpace replacing ScrollingSpace in `Workspace`: tree layout, keyboard
   focus/move by direction, drag-to-dock with insert hint, divider resize,
   fullscreen/maximize. Floating Desktop unchanged.
   - [x] dock tree, focus/move by direction, drag-to-dock, insert hint
   - [x] new windows open floating; `open-docked true` window rule docks;
     `toggle-window-floating` / `move-window-to-tiling` dock right of the
     last focused docked window; dragging a floating window onto a dock
     zone (screen edge band, edge band of a docked window) docks it
   - [x] divider drag: left-dragging the gap between two docked siblings
     (or a band around the shared edge when gaps are small) resizes exactly
     those two; col-resize / row-resize cursor on hover
   - [x] minimize keeps the slot: `minimize-window` / `restore-window`
     actions (IPC too), xdg_toplevel set_minimized; the leaf stays in the
     tree with zero size, siblings take its space; focusing the window
     (foreign-toplevel activate, `focus-window`) restores it in place.
     IPC Window has `is_minimized`. Floating windows minimize too: hidden,
     restored at the same position and size.
   - [x] inserting splits the target's share only; removing gives the share
     back to the adjacent sibling(s) proportionally
2. Camera: canvas larger than screen, zoom/pan/frame, icon tiles, history.
   - [x] canvas with minimum leaf size, camera follow/frame/show-all/zoom
     levels/pan/history, scaled rendering and input mapping, Mod+pinch,
     click/double-click while zoomed
   - [x] icon tiles: leaves seen narrower than `icon-threshold` draw as a
     rounded tile in the app's colour with its icon (desktop entry → icon
     theme: kdeglobals theme, breeze-dark, breeze, hicolor; SVG via resvg or
     PNG, loaded off-thread, redraw when ready; the app's letter otherwise);
     a subtree below threshold is one tile showing its
     leaf outlines. Clicking anywhere in a slot activates; double-click, or
     Enter on the focused tile, frames the whole tile (the collapsed
     subtree).
3. Tab groups (reuse niri's tabbed column display), saved views.
   - [x] tab groups: a leaf holds several tiles with one shown and niri's
     tab indicator; dropping a docked window on the middle of another joins
     its group (insert hint = whole target); `focus-tab-next/prev` cycle;
     clicking the indicator or activating a hidden tab shows it; dragging a
     tab's window (client move request or Mod+drag) tears it off.
     Fullscreen/maximize belong to the shown tab and drop on tab switch.
     Minimizing any tab minimizes the whole group.
   - [ ] saved views
   - [x] resting ~400 ms over the middle of a docked window arms the tab
     drop (floating and docked drags)
4. KDE appmenu protocol for hearth's global menu bar.
5. NixOS session (`nixosModules.arbor`, done) + hearth backend on the
   (niri-compatible) IPC.

## Colour management

- `color-profile "edid"` per output (default `"srgb"`, niri's behaviour): maps
  sRGB to the panel's EDID primaries with the CRTC colour pipeline
  (DEGAMMA_LUT = sRGB EOTF, CTM = sRGB->panel with Bradford to the panel white,
  GAMMA_LUT = inverse EOTF). wlr-gamma-control ramps are composed after the
  inverse EOTF, so night light still works. Math in `src/backend/color.rs`
  (unit tested), DRM side in `src/backend/tty/color_pipeline.rs`; all three
  properties are owned by `GammaProps` in tty.rs and set in one atomic commit.
- `hdr { sdr-brightness N; max-luminance N; }` per output: HDR for SDR
  content, also entirely in the CRTC pipeline (DEGAMMA = sRGB EOTF, CTM =
  sRGB->BT.2020, GAMMA_LUT = PQ with sRGB white at N nits) plus connector
  `Colorspace` BT2020_RGB, PQ `HDR_OUTPUT_METADATA`, `max bpc` 10. No shader
  pass, plane stays XRGB8888 (the pipeline and the 10 bpc link carry the
  precision). Connector props use legacy set_property like upstream's
  `reset_hdr` (allows the modeset amdgpu needs to enter/leave HDR) and are
  re-applied after VT switch.
- Not done (HDR next steps): `wp_color_management_v1` so HDR clients
  (mpv, games) can submit PQ/scRGB surfaces; that needs per-surface
  conversion in the renderer (GLES shader on each surface texture into a
  linear/PQ FP16 offscreen, then KMS GAMMA_LUT becomes identity-ish) or the
  per-plane `COLOR_PIPELINE` colorop uAPI (kernel 6.19+, amdgpu has it; only
  exposed to clients that set DRM_CLIENT_CAP_PLANE_COLOR_PIPELINE, so
  drm_info does not show it) - smithay has no support for either yet.
  Screencasts/screenshots stay sRGB (correct for SDR content).

## Title bars

arbor draws macOS-style title bars for windows that negotiated server-side
decorations (xdg-decoration server-side, or the KDE server-decoration
protocol in server mode: Qt/KDE apps). CSD apps (browsers, Electron, GTK
header bars) don't get one. The bar sits inside the tile above the window
(the window gets the tile height minus the bar), with traffic-light buttons
on the left (close, minimize, zoom; symbols on hover) and the title
centered; top corners follow `geometry-corner-radius`. Drag the bar to move
(and drag-to-dock); double-click or the green button toggles maximize.

```kdl
layout {
    title-bars {
        // off
        height 28
        active-color "#31363b"
        inactive-color "#202326"
        active-text-color "#fcfcfc"
        inactive-text-color "#a1a9b1"
    }
}
window-rule {
    match app-id="^org.example.App$"
    title-bar false
}
```

With title bars on, the decoration globals are offered to clients even
without `prefer-no-csd`, so that Qt stops drawing its own bar.

## Code map

- `src/layout/dock.rs`: DockSpace (tree, layout, render positions, hit
  testing, interactive move/resize targets).
- `src/layout/arbor.rs`, `src/layout/workspace_arbor.rs`: arbor methods
  on `Layout` / `Workspace` (minimize, restore), kept out of upstream files.
- `src/layout/title_bar.rs`, `src/input/title_bar.rs`,
  `arbor-config/src/title_bar.rs`: server-side title bars.
- `src/layout/icon_tile.rs`: icon tiles.
- `src/layout/expose.rs`: overview layout math (unit tested) and `Layout`
  entry points; `src/layout/monitor_expose.rs` (included from
  `monitor.rs`): overview rendering and hit testing; `src/input/expose.rs`:
  overview clicks, drags and hover; `src/tests/expose.rs`: headless render
  check (`cargo test --lib tests::expose -- --ignored`, needs EGL;
  `ARBOR_EXPOSE_SHOT_DIR` writes the frames as PNGs).
- `src/layout/camera.rs`: camera math (framing, panning, history);
  `src/input/camera.rs`: pinch and zoomed clicks.
- `src/input/dock_divider.rs`: divider hover cursor and drag start.
- `src/layout/workspace.rs`: holds `DockSpace` where niri held
  `ScrollingSpace`.
- `src/protocols/kde_appmenu.rs`: server side of `org_kde_kwin_appmenu`.
  The menu's DBus address shows up as `app_menu` on IPC windows
  (`WindowOpenedOrChanged` fires when it changes). Qt only exports its menu
  if `com.canonical.AppMenu.Registrar` is on the session bus when the app
  starts, so the shell has to own that name (a stub is enough).
