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
| Session | `bin/arbor-session` (starts `arbor.service`), user units `arbor.service` and `arbor-shutdown.target`, `share/wayland-sessions/arbor.desktop` (`DesktopNames=arbor;KDE;`, so `XDG_CURRENT_DESKTOP=arbor:KDE`; `arbor --session` also sets it; see "XDG_CURRENT_DESKTOP" below) |
| Portals | `share/xdg-desktop-portal/arbor-portals.conf`; on NixOS `xdg.portal.config.arbor`. arbor's own backend: `share/xdg-desktop-portal/portals/arbor.portal` (D-Bus name `org.freedesktop.impl.portal.desktop.arbor`) |
| Config | `$XDG_CONFIG_HOME/arbor/config.kdl`, override with `--config` or `$ARBOR_CONFIG`; system-wide fallback `/etc/arbor/config.kdl`. The default config is written there on first run. |
| IPC | `$XDG_RUNTIME_DIR/arbor.<wayland-display>.<pid>.sock`, exported as `$ARBOR_SOCKET`. `$NIRI_SOCKET` is exported with the same path so tools with niri IPC support (waybar's niri modules, Quickshell, ...) keep working; `arbor msg` reads `$ARBOR_SOCKET` first, then `$NIRI_SOCKET`. The protocol is niri's. |
| Env | `ARBOR_DISABLE_SYSTEM_MANAGER_NOTIFY=1` skips the systemd/fd ready notification. Spawned apps get transient scopes `app-arbor-*.scope`. Build time: `ARBOR_BUILD_COMMIT` / `ARBOR_BUILD_VERSION_STRING` set the version string. |
| Nix | `packages.<system>.arbor` (= `default`, plus `arbor-debug`), `overlays.default` adds `pkgs.arbor`, `nixosModules.arbor` (= `default`) provides `programs.arbor = { enable; package; plasmaOnlyAutostart; useNautilus; rdp; }` |

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

## XDG_CURRENT_DESKTOP

An arbor session sets `XDG_CURRENT_DESKTOP=arbor:KDE`.

- **Why KDE**: some apps only enable features for desktops they recognise. Zoom (7.1.5)
  enables Wayland screen sharing only when it recognises the desktop: it reads
  `XDG_CURRENT_DESKTOP` (and `DESKTOP_SESSION`) through `QProcessEnvironment` and runs
  `QString::indexOf(name, 0, Qt::CaseInsensitive)` for "Gnome", "KDE", "Deepin", "plasma",
  "cinnamon", "Hyprland", "sway", "Mate", "xfce", "lxde"; anything else is "Unsupported
  desktop" (found by disassembling the function that references those strings). It is a
  case-insensitive substring match, so `arbor:KDE` counts as KDE. For KDE it then expects
  `xdg-desktop-portal-kde` at the usual FHS paths (nixpkgs' `zoom-us` provides them with
  `plasma6XdgDesktopPortalSupport = true`) and uses `org.freedesktop.portal.ScreenCast`,
  which arbor serves itself. KDE rather than GNOME because the rest of the portal setup and
  the apps are KDE's.
- **Why arbor first**: xdg-desktop-portal tries `<desktop>-portals.conf` for each name in
  order, so `arbor-portals.conf` (NixOS: `xdg.portal.config.arbor`) wins over
  `kde-portals.conf`; autostart entries can opt out with `NotShowIn=arbor` (the first
  matching name decides).
- **Side effects checked**:
  - XDG autostart: entries with `OnlyShowIn=KDE` now start under arbor. Plasma's own
    components carry `X-systemd-skip=true` (plasmashell, kglobalacceld, powerdevil, kaccess,
    xembedsniproxy, gmenudbusmenuproxy, polkit-kde-agent, baloo) and stay off. The rest:
    `org.kde.plasma-fallback-session-restore` would relaunch the last Plasma session's apps
    next to arbor's own session restore, `org.kde.discover.notifier` and `kwrited` are useless
    here; the NixOS module skips these in arbor sessions
    (`programs.arbor.plasmaOnlyAutostart`, a drop-in `ExecCondition` per entry; Plasma is
    unaffected). Add your own Plasma-only entries to that list.
  - xdg-utils (`xdg-open`) matches the whole variable (`KDE)` in a `case`), so it behaves as
    before (generic, not kde-open).
  - Qt's choice of platform theme may or may not treat `arbor:KDE` as KDE (not verified);
    set `QT_QPA_PLATFORMTHEME=kde` to get the Plasma integration theme for sure.
  - Chromium/Electron split the list and can see KDE: Brave/Chrome may then default to the
    kwallet password store and offer the Qt theme; pass `--password-store=kwallet6`
    explicitly (already in use here).
  - `KDE_FULL_SESSION` and `KDE_SESSION_VERSION` are not set, so KDE apps don't assume
    Plasma (no KWin-only paths).

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

## Global shortcuts from apps

Apps (Chromium/Brave, Discord, OBS, Speech Note, ...) register global
shortcuts through the `org.freedesktop.portal.GlobalShortcuts` portal.
arbor is the backend (`org.freedesktop.impl.portal.GlobalShortcuts`,
version 1) since it owns the keyboard; select it in the portal config:

```ini
# arbor-portals.conf
org.freedesktop.impl.portal.GlobalShortcuts=arbor;
```

On NixOS the module sets `xdg.portal.config.arbor."org.freedesktop.impl.portal.GlobalShortcuts" = "arbor"`
and adds the arbor package to `xdg.portal.extraPortals` (for `arbor.portal`).
A config that sets `xdg.portal.config.arbor` with `mkForce` must add that
line itself.

- An app's shortcut gets its *preferred trigger* (XDG format `CTRL+ALT+r`
  or arbor syntax `Ctrl+Alt+R`) only when that parses and clashes with no
  config bind and no other app's shortcut. Otherwise it stays unassigned
  until you assign it. arbor shows no dialog.
- Config binds always win: portal shortcuts are only checked when no bind
  matched the key, not while locked, and not while a client inhibits
  shortcuts. The key press and release go to the app as `Activated` /
  `Deactivated` (with the key event timestamps) instead of to the focused
  window.
- Assignments are stored per app id in
  `$XDG_CONFIG_HOME/arbor/global-shortcuts.json`
  (`apps.<app id>.<shortcut id> = { description, preferred_trigger, trigger, user_cleared }`,
  plus `source: "kde"` and `app_name` for KDE shortcuts, below). This one
  store holds every app shortcut, portal and KDE alike.
  Clearing a trigger sets `user_cleared`, so the preferred trigger isn't
  re-applied when the app starts again.

| Command | Does |
| --- | --- |
| `arbor msg global-shortcuts` (`--json`) | List every app shortcut (portal and KDE): app id / KDE component, id, description, trigger, preferred trigger, whether the app is running, `source`, `app_name` |
| `arbor msg set-global-shortcut <app-id> <id> <trigger>` | Assign a trigger (fails on a clash, naming it) |
| `arbor msg set-global-shortcut <app-id> <id> --clear` | Remove the assignment |

IPC: `Request::GlobalShortcuts` → `Response::GlobalShortcuts(Vec<GlobalShortcut>)`,
`Request::SetGlobalShortcut { app_id, id, trigger: Option<String> }` →
`Handled` or an error string. A change is sent to the running app as
`ShortcutsChanged`. Unsandboxed apps can have an empty app id (use `""`).

### KDE global shortcuts (kglobalaccel)

KDE/Qt apps (Speech Note, KDE Connect, Spectacle, Kate, Konsole, ...)
register global actions with KGlobalAccel over `org.kde.kglobalaccel`.
Under Plasma kglobalacceld inside KWin grabs those keys; under arbor the
compositor serves that D-Bus API itself (`src/dbus/kglobalaccel.rs`,
`src/kglobalaccel/`) as a compatibility layer over the store above:

- `/kglobalaccel` (`org.kde.KGlobalAccel`, the KF6 interface: `doRegister`,
  `setShortcutKeys`/`setShortcut`, `setForeignShortcutKeys`, `shortcutKeys`,
  `defaultShortcutKeys`, `setInactive`, `unregister`, `getComponent`,
  `allComponents`, `allMainComponents`, `allActionsForComponent`,
  `actionList`, `globalShortcutsByKey`, `globalShortcutAvailable`,
  `blockGlobalShortcuts`, the deprecated `int` variants, signal
  `yourShortcutsChanged`) and `/component/<name>`
  (`org.kde.kglobalaccel.Component`: `globalShortcutPressed` /
  `globalShortcutReleased`, `invokeShortcut`, `allShortcutInfos`,
  `shortcutNames`, `cleanUp`, `isActive`, `friendlyName`, `uniqueName`).
- Registrations become store entries with `source: "kde"`, keyed by the
  component name. Keys come from the store; a new action gets the keys the
  app asks for (its default) if they are free: no config bind and no other
  app shortcut has them. Config binds always win, then portal shortcuts,
  then KDE ones; not while locked or while a client inhibits shortcuts.
- KGlobalAccel semantics kept: a running app's later "set my shortcut"
  calls load the stored keys back (autoloading); only actions of running
  apps take key presses, and an app that leaves the bus stops taking them.
- Components named `*.desktop` are launchers (KDE's "custom shortcuts" and
  `X-KDE-Shortcuts`): `_launch` starts the desktop file, other ids its
  `[Desktop Action]`s (found in `applications/` or `kglobalaccel/` under the
  XDG data dirs). They always take their keys.
- Not supported: shortcut contexts (`component|context` maps to the
  component), multi-key sequences, modifier-only shortcuts, the
  context-taking overloads of `shortcutNames`/`allShortcutInfos`, and
  several keys per action (the store keeps the first).
- arbor owns `org.kde.kglobalaccel` from startup (session instance). There
  is no D-Bus activation file for it, and `plasma-kglobalaccel.service` is
  only pulled in by `plasma-workspace.target`, so kglobalacceld doesn't
  start under arbor; if it did, it couldn't take the name.

**One-off import from Plasma.** The first time arbor starts without
`global-shortcuts.json`, it copies the assignments from
`$XDG_CONFIG_HOME/kglobalshortcutsrc` into it (read only; arbor never
writes that file). `arbor msg import-kde-shortcuts [--overwrite]` does it
again (IPC `Request::ImportKdeShortcuts { overwrite }` →
`Response::ImportedKdeShortcuts(count)`). Imported: app components (their
current key, if arbor can bind it), `[services]` launchers, and
Chromium/Electron shortcuts that xdg-desktop-portal-kde had stored (ids
`<32 hex>-name`), which go in as portal shortcuts under their app id.
Skipped: Plasma's own components (`kwin`, `plasmashell`, `ksmserver`,
`mediacontrol`, `kmix`, `org_kde_powerdevil`, `KDE Keyboard Layout
Switcher`, `kaccess`, `ActivityManager`, `khotkeys`, ...: arbor binds and
hearth do those jobs), the krunner/kscreen/touchpad/system monitor
launchers, unassigned actions, and keys already taken by an earlier entry.

`arbor msg global-shortcuts` and `set-global-shortcut` cover KDE entries
too (`app-id` is the component, e.g. `net.mkiol.SpeechNote`); a change is
sent to the running app as `yourShortcutsChanged`.

Test on a private bus (registers an action like KGlobalAccel, presses the
key, checks `globalShortcutPressed`; checks the signatures with busctl):
`dbus-run-session -- cargo test --lib tests::kglobalaccel -- --ignored`
(needs libwayland on `LD_LIBRARY_PATH` and a throwaway `XDG_RUNTIME_DIR`).

## Wallpaper and Inhibit portals

arbor's portal backend (`arbor.portal`, `org.freedesktop.impl.portal.desktop.arbor`)
also implements:

- **Wallpaper** (`SetWallpaperURI`): arbor draws no wallpaper, so it runs a
  command with the image path appended. The image is first copied (or, for
  http(s), downloaded with `curl`) to `$XDG_CACHE_HOME/arbor/wallpaper/`
  under a new name, so it outlives the app's document-portal grant and path
  watchers notice. `set-on` picks the command(s); `show-preview` is ignored
  (no dialog). Response 2 when no command is configured or it fails.

  ```kdl
  portal {
      wallpaper-command "hearth-ipc" "call" "wallpaper" "setImage"
      // lockscreen-wallpaper-command "..."
  }
  ```

- **Inhibit**: the `idle` flag stops arbor's idle notifications (what
  swayidle listens to) until the request is closed or the portal goes away.
  Logout/switch-user/suspend flags are accepted without effect;
  `CreateMonitor` (session state) isn't implemented. The KDE and GNOME
  backends forward to PowerDevil / gnome-session, which don't run here.

Select them: `org.freedesktop.impl.portal.Wallpaper=arbor;` and
`org.freedesktop.impl.portal.Inhibit=arbor;` (`arbor-portals.conf` and the
NixOS module do).

`org.freedesktop.ScreenSaver.Inhibit` (browsers, video players) also stops
idle notifications; changes wake the compositor loop so that an
un-inhibit reaches swayidle even when nothing else happens on screen
(before, a paused video in an idle session kept the screen awake until the
next input). Only `Inhibit`/`UnInhibit` are implemented (no `Lock`,
`GetActive`, `SimulateUserActivity`). Test: `tests::kglobalaccel::idle_inhibit_over_dbus`
(same private-bus command as above).

## Screen sharing, screenshots and remote control

arbor is its own portal backend for these (`org.freedesktop.impl.portal.desktop.arbor`,
`share/xdg-desktop-portal/portals/arbor.portal`), built straight on its PipeWire casting
and input pipeline: no xdg-desktop-portal-gnome, no Mutter D-Bus round trip. (The
Mutter-compatible `org.gnome.Mutter.ScreenCast` API is still there for anything that talks to
it directly.)

- **ScreenCast**: `Start` shows an arbor-drawn dialog on the active screen: one button per
  screen ("the entire screen: Dell U2723QE (DP-1)") and "one window (click it in the
  overview)", which opens the overview; clicking a window shares it, clicking empty space or
  Escape cancels. Number keys, arrows + Enter and clicks work in the dialog; Escape or Cancel
  closes it. Cursor modes hidden / embedded / metadata, DMA-BUF or SHM, damage-driven frames
  and window resizing are the casting code's (the same as for the Mutter API). One stream per
  session: a request for several sources gets one.
- **RemoteDesktop**: the same dialog asks to "use your keyboard" (or keyboard and mouse, ...);
  with screen-cast sources selected on the session it asks for both at once. `Notify*`
  keyboard, pointer, scroll and touch calls go through `remote_input` into the normal input
  pipeline, so binds and global shortcuts see them like real keys. Keysyms are typed with the
  key and the shift level modifiers of the active layout (locked Caps Lock respected); a
  keysym no key produces ("—", "é" on a US layout, emoji) is typed through a temporary
  keymap with one spare key, and the original keymap and modifiers come back right after.
  Calls for devices that weren't granted fail. Closing the session releases every key,
  button and touch it still holds. `ConnectToEIS` hands out a libei socket served by arbor
  (`src/eis_server.rs`, sender context): one seat with the allowed capabilities, a keyboard
  carrying arbor's keymap, relative and absolute pointers (one region per output, global
  logical coordinates), touchscreen and text (keysyms and UTF-8 are typed like
  `NotifyKeyboardKeysym`). Deskflow / Input Leap clients and other libportal users need
  this.
- **Clipboard** (with RemoteDesktop): when the app calls `RequestClipboard` before `Start`,
  the dialog asks for the clipboard too and `clipboard_enabled` comes back true. The session
  hears about every clipboard change (`SelectionOwnerChanged`, mime types, whether it set it
  itself), reads arbor's clipboard with `SelectionRead`, and takes it over with
  `SetSelection`: arbor then offers its formats to windows and forwards each paste to the
  session (`SelectionTransfer` → `SelectionWrite` hands over the paste target's fd). Works
  with the clipboard persistence (a remote selection is never copied or kept).
- **Remember**: when the app asks for persistence (`persist_mode`), the dialog has a
  "Remember, and don't ask again" checkbox (ticked by default). The choice goes back to the
  frontend as restore data (`("arbor", 1, {output | window-app-id + window-title, devices})`),
  which keeps it under the app's restore token, so later sessions start without a dialog
  (Whisper Typer's keyboard-only session, a meeting app re-sharing the same screen). A
  restored screen that is gone, a window that no longer exists, or more devices than were
  allowed before bring the dialog back.
- **InputCapture** (Deskflow / Input Leap servers): `CreateSession` asks once ("take over your
  keyboard and mouse"). Zones are the screens (global logical coordinates); barriers must lie
  on an outer screen edge (others come back in `failed_barriers`). After `Enable`, pushing the
  pointer through a barrier activates the capture: `Activated` (activation id, cursor
  position, barrier id), and from then on relative motion, buttons, scroll and keys go to the
  app over EIS (`ConnectToEIS`, receiver context, keyboard with arbor's keymap) instead of to
  windows, until the app calls `Release` (optionally with a cursor position to warp to) or
  `Disable`, or the user presses **Super+Shift+Escape** (always works; disables the
  session). Code: `src/input_capture.rs`, `src/dbus/portal_input_capture.rs`.
- **Screenshot**: non-interactive requests capture every screen into one PNG (the portal
  frontend asks permission first through the Access portal and remembers it); interactive ones
  open arbor's screenshot UI (Escape cancels). Files go to `screenshot-path`, or
  `~/.cache/arbor/screenshots` when screenshots aren't saved. `PickColor` uses the colour
  picker.

Code: `src/dbus/portal_casting.rs` (ScreenCast + RemoteDesktop, shared sessions),
`src/dbus/portal_screenshot.rs`, `src/ui/portal_dialog.rs`, `src/remote_input.rs`,
`State::start_portal_cast` in `src/screencasting/mod.rs`. Tests: `remote_input` and
`portal_dialog` / `input_capture` unit tests; `tests::portal_input_capture` (a libei receiver
through barrier, activation, captured motion and keys, release) and `tests::portal_remote_desktop` drive the D-Bus API on a private
bus (dialog, restore data, keycodes and keysyms reaching the seat keyboard, release on close,
a libei sender typing through `ConnectToEIS`, clipboard sharing both ways):

```sh
ARBOR_TEST_PRIVATE_DBUS=1 dbus-run-session -- cargo test portal_ -- --ignored --test-threads=1
```

## Desktop integration

What KDE Plasma provided to apps, and what serves it under arbor + hearth.
Portal backends assume the portal config `default=kde;gtk`, Secret
`kwallet`, and ScreenCast / Screenshot / RemoteDesktop / Clipboard / InputCapture /
GlobalShortcuts / Wallpaper / Inhibit `arbor` (see "Screen sharing, screenshots and remote
control"). Nothing needs xdg-desktop-portal-gnome.

| Portal | Backend under arbor | Works outside KWin? |
| --- | --- | --- |
| Access | kde | Yes (Qt dialog) |
| Account | kde | Yes |
| AppChooser | kde | Yes |
| Background | kde | Partly: autostart requests work; the running-apps list comes from KWin's window management protocol, which arbor doesn't offer, so apps are never seen as running in the background |
| Clipboard | **arbor** | Yes: clipboard sharing for remote-desktop sessions (both directions) |
| DynamicLauncher | kde | Yes |
| Email | kde | Yes |
| FileChooser | kde | Yes (KDE file dialog; needs KIO, installed) |
| GlobalShortcuts | **arbor** | Yes (above). The kde backend would also work now, through arbor's kglobalaccel |
| Inhibit | **arbor** | Yes for idle (above). kde (PowerDevil) and gnome (gnome-session) don't run here |
| InputCapture | **arbor** | Yes: barriers on outer screen edges, capture over EIS (Deskflow / Input Leap servers) |
| Notification | kde | Yes: KNotification → `org.freedesktop.Notifications` (swaync, started by hearth) |
| OpenURI | kde | Yes |
| Print | kde | Yes |
| ProxyResolver | frontend only | Yes (no backend involved) |
| RemoteDesktop | **arbor** | Yes: keyboard, pointer, touch through the Notify* methods or EIS (`ConnectToEIS`) |
| ScreenCast | **arbor** | Yes: a screen or a window, arbor-drawn picker, restore tokens |
| Screenshot | **arbor** | Yes: whole desktop, or interactive through the screenshot UI; PickColor |
| Secret | kwallet | Yes (ksecretd/kwalletd; `pam_kwallet_init` is started by the config) |
| Settings | kde | Yes: reads kdeglobals (colour scheme, fonts, accent) |
| Usb | none | No: neither kde nor gtk implements `impl.portal.Usb` |
| Wallpaper | **arbor** | Yes with `portal { wallpaper-command }` (above). kde's needs plasmashell |

| Service | Under arbor |
| --- | --- |
| `org.kde.kglobalaccel` (global shortcuts of KDE apps) | arbor (above) |
| `org.kde.KWin` | Not provided, on purpose (arbor and hearth don't depend on KDE tools). Spectacle needs KWin's ScreenShot2 and zkde_screencast, so it doesn't work here; hearth has its own Spectacle-style tool (Screenshots below). Also matters for KWin scripts, "show desktop"/window lists over KWin D-Bus, krunner's window runner. Window activation goes through xdg-activation, which arbor implements |
| `org.freedesktop.ScreenSaver` | arbor: Inhibit/UnInhibit stop idle notifications (swayidle honours them through ext-idle-notify); `Lock`/`GetActive`/`SimulateUserActivity` not implemented. Locking goes through logind (`loginctl lock-session` → swayidle `lock` → hearth's lock screen, hyprlock as its fallback; a dead lock client leaves the session locked until a new locker takes over), which KDE Connect uses |
| `org.freedesktop.Notifications` | swaync (hearth) |
| Brightness / volume / media keys (PowerDevil, kmix, mediacontrol) | arbor config binds → hearth IPC, wpctl, playerctl. Apps calling PowerDevil's D-Bus get nothing |
| xdg-activation / startup feedback | arbor implements xdg-activation-v1 (focus stealing prevention, `XDG_ACTIVATION_TOKEN` for spawned apps). There is no launch feedback (busy cursor, dock bounce) in arbor; that would be hearth's dock |
| KDE Connect | kdeconnectd + `kdeconnect-indicator` (started by the config): notification sync, MPRIS, clipboard (wlr-data-control), lock (logind), screensaver inhibit, its global shortcuts (kglobalaccel) work; remote input goes through arbor's RemoteDesktop portal |
| StatusNotifierItem (tray) | hearth is the watcher and host |
| `com.canonical.AppMenu.Registrar` (global menu) | hearth (see Code map) |
| kded modules (plasma-nm secret agent, bluedevil, printer and disk notifiers, keyboard layout OSD) | kded6 doesn't run. hearth has a Bluetooth agent; without plasma-nm's agent, Wi-Fi/VPN password prompts need another NetworkManager secret agent |

### Screenshots and screen recording

hearth's screenshot tool (Print; Settings > Screenshots in hearth) stands in
for Spectacle. Outputs and regions come from wlr-screencopy (Quickshell's
frozen `ScreencopyView`, grim), recordings from wf-recorder over the same
protocol. Windows come from arbor itself, `Action::ScreenshotWindow`
(`arbor msg action screenshot-window --id N --path P ...`):

- `decorations` (default false): render the whole tile, i.e. arbor's title
  bar, border and shadow, not just the window's surfaces. Off: the window
  alone, its visual geometry at the origin.
- `shadow` (default true): keep shadows (arbor's with `decorations`, the
  client's own drawn shadow otherwise). Off crops the image to the tile or
  the window geometry.
- `clipboard` (default true): set the PNG as the clipboard selection and
  show the "Screenshot captured" notification. Tools that handle the image
  themselves pass false.

The window is rendered by itself, so it captures even when covered or on
another workspace, at its output's scale (4/3 gives 4/3-sized pixels, no
resampling), in SDR like the other screenshots. With an explicit `--path`
the PNG is written to `<path>.part` and renamed, so a caller waiting for the
file never reads half of it. Code: `Arbor::screenshot_window` and
`WindowScreenshotOptions` in `src/arbor.rs`.

## Window previews (window capture)

Shells capture single windows with `hyprland_toplevel_export_manager_v1`
(version 2): this is what Quickshell's `ScreencopyView { captureSource:
<Toplevel> }` uses (hearth's dock hover previews, overview and switcher
thumbnails). `capture_toplevel_with_wlr_toplevel_handle` takes a
`zwlr_foreign_toplevel_handle_v1`; the plain `capture_toplevel` takes the
arbor window id from IPC. Frames are the window geometry at its output's
scale (no title bar, popups clipped), Argb8888 in shm or dmabuf buffers.
Copies without `ignore_damage` wait for damage since the previous copy made
through the same manager, checked on every output redraw, so docked,
floating, minimized windows, hidden tabs and windows on other workspaces
all capture (hidden ones show their last content until they commit again).
The cursor is never drawn in. Code: `src/protocols/toplevel_export.rs`,
`src/handlers/toplevel_export.rs`; tests in `src/tests/toplevel_export.rs`
(the rendering one is `--ignored`, needs EGL). Not implemented:
ext-image-copy-capture (Quickshell 0.3.2 uses it for outputs only).

## Session restore

Like Plasma's "restore previous session": the running apps are saved to
`$XDG_STATE_HOME/arbor/session.json` (`~/.local/state/arbor/session.json`)
when arbor quits (also on SIGTERM, i.e. logout) and every 3 minutes, and
reopened at the next session start (`arbor --session`, after 2 s).

- Saved per window: app id, launch command (the `Exec` of
  `<app-id>.desktop` from the XDG data dirs, else the process command line
  from `/proc`), workspace (output + index, or name), the workspace's whole
  dock tree (splits, fractions, tab groups and the shown tab) or the
  floating position and size, and maximized / fullscreen / minimized.
  Dialogs (windows with a parent) are left out.
- One launch per saved process: an app with several windows in one
  process (browsers, Dolphin, Konsole) is launched once and is expected to
  reopen its windows itself.
- As windows map they're matched by app id, in saved order, and put back;
  the dock tree is rebuilt with the windows that have arrived so far
  (missing ones are skipped, extra docked windows stay beside it). Windows
  that haven't appeared after 90 s are forgotten.
- Automatic saves never replace a saved session with an empty one (at
  logout the apps may close before arbor); `arbor msg session save` does.

```kdl
session-restore {
    // off
    exclude-app-ids "org.keepassxc.KeePassXC"
}
```

`arbor msg session save|restore|clear` (also the actions `session-save`,
`session-restore`, `session-clear`). xx-session-management (apps
restoring their own windows) is not implemented; smithay has no support.
Code: `src/session.rs`, `src/layout/dock_shape.rs`; tests in
`src/tests/session.rs`.

## Clipboard after the app quits

Like Klipper: arbor reads every format of each new clipboard and primary
selection in the background, and when the owner goes away (the app quits
or drops its source and nobody else took over) offers its copy as a
compositor selection, so pasting still works. Selections offering
`x-kde-passwordManagerHint` (KeePassXC and other password managers) are
never kept. The copy is capped (all formats together); formats that don't
fit are left out. arbor never takes the selection while a client owns one,
so data-control clipboard managers (Vicinae's history) are unaffected: they
just see the restored selection as one more change with the same content.

```kdl
clipboard {
    // persist false
    max-size-mb 32
}
```

Code: `src/clipboard_persist.rs`; tests in `src/tests/clipboard_persist.rs`.

## Accessibility

- **Screen zoom** (KWin Zoom): `zoom-in`, `zoom-out`, `zoom-reset`
  scale each output's whole frame around the pointer, animated (with the
  `horizontal-view-movement` animation). Scaling around the pointer keeps
  the pointer over what it points at, so input needs no remapping, and
  moving the pointer pans. It is applied after everything else, independent
  of the dock camera; screencasts and screenshots stay unzoomed. Allowed on
  the lock screen. Following the text caret is not implemented yet.
- **Reduce motion**: `accessibility { reduce-motion }`, or at runtime
  `arbor msg action set-reduce-motion true|false` /
  `toggle-reduce-motion`, turns animations off (as `animations { off }`).
- **Screen reader**: Orca talks to apps over AT-SPI; the compositor's part
  is `org.freedesktop.a11y.KeyboardMonitor` (the KWin/Mutter interface Orca
  48+ uses to grab its modifier key, Insert / Caps Lock, and to watch
  keys), which arbor serves on the session bus when started with
  `--session`, plus AccessKit for arbor's own UI (overview, Alt-Tab,
  screenshot UI, exit dialog). Apps need accessibility on: GTK always, Qt
  with `QT_ACCESSIBILITY=1` (or Qt 6 when the AT-SPI bus says a screen
  reader is active), Electron/Chromium with `--force-renderer-accessibility`
  or when Orca is running. Not tested with Orca in a live session.

```kdl
accessibility {
    zoom-step 1.25   // factor per step
    zoom-max 16
    // reduce-motion
}
binds {
    Mod+Alt+Equal { zoom-in; }
    Mod+Alt+Minus { zoom-out; }
    Mod+Alt+0 { zoom-reset; }
}
```

Code: `src/zoom.rs`; render check in `src/tests/zoom.rs` (`--ignored`, EGL).

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
| `portal` | arbor's portal backend: `wallpaper-command`, `lockscreen-wallpaper-command` (see Wallpaper and Inhibit portals) |
| `clipboard`, `session-restore`, `accessibility` | arbor: see the sections above |
| `animations`, `gestures` (hot corners, DnD workspace switch), `overview`, `cursor`, `environment`, `spawn-at-startup` / `spawn-sh-at-startup`, `workspace "name"`, `screenshot-path`, `hotkey-overlay`, `prefer-no-csd`, `xwayland-satellite`, `debug` | as in niri |

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
  `zoom-in`, `zoom-out`, `zoom-reset`, `set-reduce-motion`, `toggle-reduce-motion`,
  `session-save`, `session-restore`, `session-clear`,
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
- `src/global_shortcuts.rs`: the app shortcut store (portal and KDE);
  `src/dbus/portal_global_shortcuts.rs`, `src/dbus/kglobalaccel.rs`: its
  D-Bus faces; `src/kglobalaccel/`: KGlobalAccel semantics, Qt key codes
  (`qt_keys.rs` is generated from Qt's tables), the one-off
  `kglobalshortcutsrc` import, desktop-file launchers.
- `src/dbus/portal_wallpaper.rs`, `src/dbus/portal_inhibit.rs`: the
  Wallpaper and Inhibit portal backends.
