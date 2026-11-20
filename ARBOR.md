# arbor

A fork of [niri](https://github.com/YaLTeR/niri) (v26.04) that replaces
scrollable tiling with a **nested docking layout under a zoomable camera**.
It's an original window manager built around docking, nesting, and zooming.

Upstream is the `upstream` remote. Rebase `main` onto new niri tags; keep
arbor changes in new files where possible so rebases stay small.

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
  - the middle of a docked window, after a short pause: tab group (later).
  - empty Desktop area: floats.
  An insert hint shows the target rectangle while dragging.
- **Dividers:** dragging the gap between two siblings resizes just those two.
- **Minimize:** a docked window's siblings take its space; restoring puts it
  back in the same slot (the tree remembers a placeholder).
- **Double-click title / maximize:** frame that window on its own (camera),
  again to return.

## Camera (milestone 2)

The dock tree is laid out at its own *canvas size*, which may be larger than
the screen (each leaf has a minimum useful size). A 2D camera (offset +
zoom) maps canvas to screen, reusing the overview's scaled rendering path.

- Zoom out / in a level, frame a node, frame a rectangle (drag with a
  modifier), neighbours left/right/up/down, back/forward history.
- Leaves rendered below a size threshold become an app-icon tile; a whole
  subtree below threshold becomes one tile showing its split lines.
  Double-click one to zoom into it.
- One animation curve for split, dock, close, and camera moves.

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
     IPC Window has `is_minimized`. Floating windows can't minimize yet.
   - [x] inserting splits the target's share only; removing gives the share
     back to the adjacent sibling(s) proportionally
2. Camera: canvas larger than screen, zoom/pan/frame, icon tiles, history.
3. Tab groups (reuse niri's tabbed column display), saved views.
4. KDE appmenu protocol for hearth's global menu bar.
5. NixOS session + hearth backend for niri IPC.

## Code map

- `src/layout/dock.rs`: DockSpace (tree, layout, render positions, hit
  testing, interactive move/resize targets).
- `src/layout/arbor.rs`, `src/layout/workspace_arbor.rs`: arbor methods
  on `Layout` / `Workspace` (minimize, restore), kept out of upstream files.
- `src/input/dock_divider.rs`: divider hover cursor and drag start.
- `src/layout/workspace.rs`: holds `DockSpace` where niri held
  `ScrollingSpace`.
