# arbor

A Wayland compositor with a **nested docking layout under a zoomable
camera**: an original window manager built around docking, nesting and
zooming. It started as a fork of [niri](https://github.com/YaLTeR/niri)
(v26.04) by Ivan Molodetskikh and contributors and is now developed on its
own: it no longer follows upstream, and niri's scrollable tiling is gone.
It keeps niri's IPC protocol and most of its config format.

## Names

The crates are `arbor` (compositor binary and library), `arbor-config` and
`arbor-ipc`. Names that other programs rely on stay niri's: the
`$NIRI_SOCKET` export (below), the IPC protocol itself, and the `niri_*`
uniform names available to custom shaders.

| What | Name |
| --- | --- |
| Binary | `arbor`: `arbor msg`, `arbor validate`, `arbor --session`, `arbor completions`. No `niri` binary. |
| Session | `bin/arbor-session` (starts `arbor.service`), user units `arbor.service` and `arbor-shutdown.target`, `share/wayland-sessions/arbor.desktop` (`DesktopNames=arbor`, so `XDG_CURRENT_DESKTOP=arbor`; `arbor --session` also sets it) |
| Portals | `share/xdg-desktop-portal/arbor-portals.conf`; on NixOS `xdg.portal.config.arbor` |
| Config | `$XDG_CONFIG_HOME/arbor/config.kdl`, override with `--config` or `$ARBOR_CONFIG`; system-wide fallback `/etc/arbor/config.kdl`. The default config is written there on first run. |
| IPC | `$XDG_RUNTIME_DIR/arbor.<wayland-display>.<pid>.sock`, exported as `$ARBOR_SOCKET`. `$NIRI_SOCKET` is exported with the same path so tools with niri IPC support (waybar's niri modules, Quickshell, ...) keep working; `arbor msg` reads `$ARBOR_SOCKET` first, then `$NIRI_SOCKET`. The protocol is niri's. |
| Env | `ARBOR_DISABLE_SYSTEM_MANAGER_NOTIFY=1` skips the systemd/fd ready notification. Spawned apps get transient scopes `app-arbor-*.scope`. Build time: `ARBOR_BUILD_COMMIT` / `ARBOR_BUILD_VERSION_STRING` set the version string. |
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

- A workspace holds a **dock tree** plus a floating space
  (the "Desktop": windows float freely there).
- Dock tree nodes are `Split { axis, children, fractions }` or `Leaf(Tile)`.
  Children of a split never overlap; dividers sit in the gaps.
- **Nothing moves until you dock something.** New windows open floating
  (on the Desktop) unless a window rule says `open-docked`.
- **Docking by drag:** drag a window by its title bar (client-side move
  requests work too) over:
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
niri's, so shells and binds keep working. Workspaces never zoom out (the
`overview { zoom }` setting is gone). Touch: tapping a window or empty space works; dragging by touch
still uses the plain interactive move (full size).

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

## Liquid glass

Shells can ask arbor to draw macOS 26-style liquid glass under parts of their
layer-shell surfaces: real refraction of whatever is behind (wallpaper,
windows, video), not a shell-side imitation. For each registered shape arbor
captures the backdrop under it (everything rendered below that surface on that
output), then draws it through a lens shader
(`src/render_helpers/shaders/glass.frag`) before the surface's own content:

- a rounded rectangle with continuous (squircle) corners, the same curve as
  hearth's `shaders/glass.frag`;
- inside, the backdrop bends inwards in a band along the rim (the *bezel*,
  by default as wide as the corner radius): strongest at the edge, nothing in
  the middle. The displacement is quadratic across the band and capped so the
  mapping never folds (no mirror images): the backdrop is magnified towards
  the rim like through the rounded edge of a thick lens;
- slight chromatic dispersion where it bends (red bends least, blue most);
- dual-kawase blur of the captured backdrop (the existing blur passes), a
  saturation boost (vibrancy) and a tint;
- a specular rim: a ~1.5 px highlight lit from the top-left with a weaker
  reflection at the bottom-right, plus light gathered in the bending band.

Variants (Apple HIG Materials): **regular** (frosted: blur 8 px, saturation
1.6, faint white tint) for menus, popovers, the dock; **clear** (blur 1 px,
saturation 1.3, slight dimming) for glass over media.

A surface with glass shapes draws no layer-rule `background-effect` and no
layer-rule `shadow` (they are rectangles around the whole surface); the shell
draws its own shadow around the shapes. Glass is drawn for surfaces on the
bottom, top and overlay layers (not the background layer).

### Registering shapes (IPC)

```json
{"SetGlass": {"namespace": "hearth-dock", "output": "DP-1", "key": "",
  "popup": false, "surface_size": [3840, 120],
  "shapes": [{"x": 1200, "y": 20, "width": 1440, "height": 86, "radius": 26,
              "params": {"variant": "Regular", "strength": 1.0}}]}}
```

- Target: the mapped layer surface(s) with that `namespace` on `output`
  (every output when `output` is null/absent). Shapes are in that surface's
  logical, surface-local coordinates.
- Several surfaces with the same namespace on one output: `surface_size`
  (logical) picks the one with that size (within 1 px); without it the
  shapes apply to all of them. With a single match the size is ignored (it
  may lag a resize by a frame).
- `popup: true` targets the xdg pop-ups of the matching layer surface
  instead (tooltips, submenus); shapes are pop-up-surface-local and
  `surface_size` picks the pop-up when several are open.
- `key`: a client can keep several independent lists per namespace (e.g.
  one per window); a request replaces only the list with the same
  (namespace, output, popup, key).
- An empty `shapes` list clears that list. Lists also go away when no mapped
  layer surface matches them any more (the surface unmapped), so a shell
  that crashes leaves nothing behind.
- `params` (all optional) override the `glass {}` defaults: `variant`
  (`Regular`/`Clear`), `strength` (0..1, scales everything), `refraction`
  (0..1), `bezel` (px), `dispersion` (0..1), `blur_radius` (px), `saturation`,
  `rim` (0..1), `tint` (straight RGBA 0..1), `opacity` (0..1, for fading the
  glass with the client's content).
- At most 64 shapes per list; replies `{"Ok":"Handled"}`. A compositor
  without glass replies an error (unknown variant), which shells can use to
  detect the feature.

Element ids are kept per shape index, so updating positions every frame
(a magnifying dock) only damages the old and new rectangles.

Testing from a shell:

```sh
arbor msg glass --namespace hearth-dock --shape 1200,20,1440,86,26
arbor msg glass --namespace hearth-dock --shape 10,10,300,60,30 --variant clear --strength 0.6
arbor msg glass --namespace hearth-dock            # clears
```

### Defaults (config)

```kdl
glass {
    // off                 // draw no glass (surfaces fall back to their layer rules)
    strength 1.0
    refraction 0.8
    bezel 0                // 0 = the shape's corner radius (at least 12 px)
    dispersion 0.12
    blur-radius 8
    clear-blur-radius 1
    saturation 1.6
    clear-saturation 1.3
    rim 0.7
    tint "#ffffff0c"
    clear-tint "#00000014"
}
```

### Rendering, damage, performance

- Each shape is one framebuffer-effect render element (`GlassElement`,
  `src/render_helpers/glass.rs`). smithay's damage tracker calls its
  capture only when something behind the shape changed (or the shape
  moved): the backdrop is blitted into a shape-sized texture at the output's
  physical resolution and blurred there (the blur cost scales with the
  shape, not the output). Glass over a static backdrop costs nothing per
  frame; glass over a playing video recaptures with each video frame, and
  only the glass rectangles are redrawn.
- Fractional scale: shape edges are snapped to the physical pixel grid
  (each edge rounded on its own), the SDF is evaluated in logical px and
  antialiased over half a physical pixel.
- Code: registry and matching in `src/glass.rs` (unit tested), defaults and
  resolution in `arbor-config/src/glass.rs`, IPC types `SetGlass`,
  `GlassShape`, `GlassParams` in `arbor-ipc`. Headless render check:
  `cargo test --lib tests::glass -- --ignored` (needs EGL;
  `ARBOR_GLASS_SHOT_DIR` writes the frames as PNGs): stripes behind a glass
  shape are displaced near the rim and untouched in the middle, at scale 1
  and 4/3.

## Configuration

The config is KDL (`$XDG_CONFIG_HOME/arbor/config.kdl`); arbor writes a
commented default there on first run (`resources/default-config.kdl` in this
repo) and reloads the file live when it changes. `arbor validate [-c FILE]`
checks a config without starting anything. `include "other.kdl"` (and
`include optional=true "..."`) splices in other files.

Top-level sections, mostly as in niri:

| Section | What |
| --- | --- |
| `input` | keyboard (xkb, repeat, numlock), touchpad, mouse, trackpoint, tablet, touch; `focus-follows-mouse`, `warp-mouse-to-focus`, `workspace-auto-back-and-forth`, `mod-key` |
| `output "NAME"` | `mode`, `scale`, `transform`, `position`, `variable-refresh-rate`, `off`; arbor: `color-profile`, `hdr { ... }` (see Colour management) |
| `layout` | `gaps`, `struts`, `focus-ring`, `border`, `shadow`, `tab-indicator` (tab groups), `insert-hint` (drag-to-dock preview), `preset-column-widths` / `preset-window-heights` (what `switch-preset-*` cycles through), `default-column-width` (new floating windows), `background-color`, `empty-workspace-above-first`; arbor: `camera { ... }`, `title-bars { ... }` |
| `window-rule` | `match` / `exclude` on app-id, title, state; `open-floating`, arbor's `open-docked`, `open-maximized`, `open-fullscreen`, `open-on-output` / `open-on-workspace`, sizes, `geometry-corner-radius`, `clip-to-geometry`, `opacity`, `block-out-from`, `title-bar false`, ... |
| `layer-rule` | the same idea for layer-shell surfaces (panels, wallpapers) |
| `glass` | arbor: liquid glass defaults (see Liquid glass) |
| `binds` | key, mouse button, wheel and touchpad-scroll binds to actions (below) |
| `animations`, `gestures` (hot corners, DnD workspace switch), `overview`, `cursor`, `environment`, `spawn-at-startup` / `spawn-sh-at-startup`, `workspace "name"`, `screenshot-path`, `hotkey-overlay`, `clipboard`, `prefer-no-csd`, `xwayland-satellite`, `debug` | as in niri |

Actions (for `binds` and `arbor msg action ...`):

- **Docked windows, by direction:** `focus-column-left/right`,
  `focus-window-up/down` (and the `-or-monitor-*`, `-or-workspace-*`
  variants), `move-column-left/right`, `move-window-up/down`: focus or move
  through the dock tree. `consume-or-expel-window-left/right` stack a window
  below its neighbour or move it out of a vertical stack;
  `consume-window-into-column`, `expel-window-from-column` likewise.
  `swap-window-left/right`, `focus-column-first/last`, `focus-column N`
  (Nth leaf in tree order).
- **Sizes:** `set-column-width` / `set-window-width` and
  `set-window-height` (`"+5%"`, `"-10"`, `"50%"`, ...) move the dividers
  around the window; `switch-preset-*` cycle the presets;
  `reset-window-height` equalizes a vertical stack;
  `maximize-window-to-edges`, `fullscreen-window`,
  `toggle-windowed-fullscreen`.
- **Docking and floating:** `toggle-window-floating` (dock beside the last
  focused docked window, or float), `move-window-to-floating` /
  `move-window-to-tiling`, `focus-floating` / `focus-tiling` /
  `switch-focus-between-floating-and-tiling`, `center-window` /
  `center-column` (floating windows).
- **arbor:** `minimize-window`, `restore-window`, `focus-tab-next/prev`,
  `camera-zoom-out/in`, `camera-show-all`, `camera-frame-window`,
  `camera-pan-left/right/up/down`, `camera-back/forward`.
- **Workspaces, monitors, overview, screenshots, misc:** as in niri
  (`focus-workspace`, `move-window-to-workspace`, `move-column-to-monitor-*`,
  `toggle-overview`, `screenshot`, `spawn`, `quit`, ...). `arbor msg action
  --help` lists every action.

Removed with niri's scrolling layout (configs using them fail to load):
the actions `focus-window-in-column`, `move-column-to-first/last/index`,
`toggle-column-tabbed-display`, `set-column-display`,
`center-visible-columns`, `maximize-column`,
`expand-column-to-available-width`; the options
`layout { center-focused-column; always-center-single-column;
default-column-display }`, the `default-column-display` window rule,
`gestures { dnd-edge-view-scroll }` and `overview { zoom }`.
`focus-follows-mouse max-scroll-amount=...` still parses but has no effect.

## Code map

- `src/layout/dock.rs`: DockSpace (tree, layout, render positions, hit
  testing, interactive move/resize targets).
- `src/layout/dock_ops.rs`, `src/layout/workspace_dock.rs`: dock-tree
  operations on `Layout` / `Workspace` (minimize, restore, tabs, title bars,
  camera).
- `src/layout/sizing.rs`: column widths, working area and toplevel bounds
  shared by the dock tree and the floating space.
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
- `src/layout/workspace.rs`: a workspace: the `DockSpace` (`dock`) and the
  `FloatingSpace` (the Desktop).
- `src/arbor.rs`: compositor state (`State`, `Arbor`); `src/input/`: input
  handling and bind dispatch; `arbor-config/`: config parsing;
  `arbor-ipc/`: IPC types.
- `src/glass.rs`, `src/render_helpers/glass.rs`,
  `src/render_helpers/shaders/glass.frag`, `arbor-config/src/glass.rs`:
  liquid glass (see Liquid glass).
- `src/protocols/kde_appmenu.rs`: server side of `org_kde_kwin_appmenu`.
  The menu's DBus address shows up as `app_menu` on IPC windows
  (`WindowOpenedOrChanged` fires when it changes). Qt only exports its menu
  if `com.canonical.AppMenu.Registrar` is on the session bus when the app
  starts, so the shell has to own that name (a stub is enough).
