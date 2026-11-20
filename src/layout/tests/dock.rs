//! Unit tests for the arbor dock tree (`DockSpace`).

use super::*;
use crate::layout::dock::{DockInsert, DockSide, DockSpace, DockTarget};
use crate::layout::monitor::InsertPosition;
use crate::utils::transaction::Transaction;

/// Dock space without a minimum leaf size, so the canvas is always the screen.
fn space() -> DockSpace<TestWindow> {
    let mut options = Options::default();
    options.layout.camera.min_leaf_width = 0.;
    options.layout.camera.min_leaf_height = 0.;
    space_with(options)
}

fn space_with(options: Options) -> DockSpace<TestWindow> {
    let view_size = Size::from((1280., 720.));
    let options = Rc::new(options);
    DockSpace::new(
        view_size,
        Rectangle::from_size(view_size),
        1.,
        Clock::with_time(Duration::ZERO),
        options,
    )
}

fn tile(space: &DockSpace<TestWindow>, id: usize) -> Tile<TestWindow> {
    let window = TestWindow::new(TestWindowParams::new(id));
    Tile::new(
        window,
        Size::from((1280., 720.)),
        1.,
        space.clock().clone(),
        space.options().clone(),
    )
}

fn add(space: &mut DockSpace<TestWindow>, id: usize) {
    let tile = tile(space, id);
    space.add_tile(None, tile, true, ColumnWidth::Proportion(0.5), false, None);
    space.verify_invariants();
}

fn dock(space: &mut DockSpace<TestWindow>, id: usize, target: usize, side: DockSide) {
    let leaf = space.leaf_id_of(&target).unwrap();
    let tile = tile(space, id);
    let insert = DockInsert {
        target: DockTarget::Leaf(leaf),
        side,
    };
    space.add_tile_docked(tile, insert, true);
    space.verify_invariants();
}

fn remove(space: &mut DockSpace<TestWindow>, id: usize) {
    space.remove_tile(&id, Transaction::new());
    space.verify_invariants();
}

fn active(space: &DockSpace<TestWindow>) -> usize {
    *space.active_window().unwrap().id()
}

#[test]
fn first_tile_fills_working_area() {
    let mut s = space();
    add(&mut s, 1);
    assert_eq!(s.tree_string(), "1");
    // Default gaps are 16.
    assert_eq!(
        s.window_rect(&1).unwrap(),
        Rectangle::new(Point::from((16., 16.)), Size::from((1248., 688.)))
    );
}

#[test]
fn add_docks_right_of_active_and_joins_parent_split() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    assert_eq!(s.tree_string(), "H[1,2]");
    assert_eq!(active(&s), 2);

    let r1 = s.window_rect(&1).unwrap();
    let r2 = s.window_rect(&2).unwrap();
    assert_eq!(r1.size.w, 616.);
    assert_eq!(r2.loc.x, 16. + 616. + 16.);

    // The active leaf's parent is already horizontal: join as a sibling, splitting the target's
    // share and leaving the other siblings alone.
    add(&mut s, 3);
    assert_eq!(s.tree_string(), "H[1,2,3]");
    let w: Vec<f64> = (1..=3)
        .map(|id| s.window_rect(&id).unwrap().size.w)
        .collect();
    assert_eq!(w, [608., 304., 304.]);

    // Focus the first leaf and add: goes right after it.
    s.focus_column_first();
    add(&mut s, 4);
    assert_eq!(s.tree_string(), "H[1,4,2,3]");
}

#[test]
fn remove_collapses_splits() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    dock(&mut s, 4, 2, DockSide::Bottom);
    assert_eq!(s.tree_string(), "H[1,V[2,4],3]");

    remove(&mut s, 4);
    assert_eq!(s.tree_string(), "H[1,2,3]");

    // Removing the active leaf activates its nearest neighbor.
    s.activate_window(&2);
    remove(&mut s, 2);
    assert_eq!(s.tree_string(), "H[1,3]");
    assert!(matches!(active(&s), 1 | 3));

    remove(&mut s, 1);
    assert_eq!(s.tree_string(), "3");
    assert_eq!(
        s.window_rect(&3).unwrap(),
        Rectangle::new(Point::from((16., 16.)), Size::from((1248., 688.)))
    );

    remove(&mut s, 3);
    assert!(s.is_empty());
    assert!(s.active_window().is_none());
}

#[test]
fn nested_same_axis_is_flattened() {
    let mut s = space();
    add(&mut s, 1);
    dock(&mut s, 2, 1, DockSide::Bottom);
    dock(&mut s, 3, 2, DockSide::Right);
    assert_eq!(s.tree_string(), "V[1,H[2,3]]");

    // Removing 1 leaves a single horizontal child, which becomes the root.
    remove(&mut s, 1);
    assert_eq!(s.tree_string(), "H[2,3]");

    // Docking left of 2 joins the root split.
    dock(&mut s, 4, 2, DockSide::Left);
    assert_eq!(s.tree_string(), "H[4,2,3]");
}

#[test]
fn focus_by_direction() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    dock(&mut s, 3, 2, DockSide::Bottom);
    assert_eq!(s.tree_string(), "H[1,V[2,3]]");
    assert_eq!(active(&s), 3);

    assert!(s.focus_up());
    assert_eq!(active(&s), 2);
    assert!(!s.focus_up());
    assert!(!s.focus_right());

    assert!(s.focus_down());
    assert_eq!(active(&s), 3);

    assert!(s.focus_left());
    assert_eq!(active(&s), 1);
    assert!(!s.focus_left());
    assert!(!s.focus_up());

    assert!(s.focus_right());
    assert!(matches!(active(&s), 2 | 3));
}

#[test]
fn move_by_direction_swaps() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    dock(&mut s, 3, 2, DockSide::Bottom);
    assert_eq!(active(&s), 3);

    assert!(s.move_left());
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[3,V[2,1]]");
    // The moved window stays focused.
    assert_eq!(active(&s), 3);
    assert!(!s.move_left());

    s.activate_window(&1);
    assert!(s.move_up());
    assert_eq!(s.tree_string(), "H[3,V[1,2]]");
    assert!(!s.move_up());
    s.verify_invariants();
}

#[test]
fn set_width_adjusts_fractions() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    // Available width: 1248 - 2 * 16 = 1216.
    s.set_window_width(Some(&1), SizeChange::SetProportion(50.));
    s.verify_invariants();
    let w1 = s.window_rect(&1).unwrap().size.w;
    let w2 = s.window_rect(&2).unwrap().size.w;
    let w3 = s.window_rect(&3).unwrap().size.w;
    assert!((w1 - 608.).abs() <= 1., "{w1}");
    // Others shrink proportionally (they were equal, so they stay equal).
    assert!((w2 - w3).abs() <= 1., "{w2} {w3}");
    assert!((w1 + w2 + w3 + 32. - 1248.).abs() <= 1.);

    s.set_window_width(Some(&1), SizeChange::AdjustFixed(-100));
    let w1b = s.window_rect(&1).unwrap().size.w;
    assert!((w1b - (w1 - 100.)).abs() <= 1., "{w1b}");

    // Clamped: other children keep a minimum size.
    s.set_window_width(Some(&1), SizeChange::SetFixed(100_000));
    s.verify_invariants();
    assert!(s.window_rect(&2).unwrap().size.w >= 49.);
    assert!(s.window_rect(&3).unwrap().size.w >= 49.);

    // Height of a leaf with no vertical ancestor: no-op.
    let before = s.window_rect(&1).unwrap();
    s.set_window_height(Some(&1), SizeChange::SetFixed(100));
    assert_eq!(s.window_rect(&1).unwrap(), before);
}

#[test]
fn set_height_uses_nearest_vertical_split() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    dock(&mut s, 3, 2, DockSide::Bottom);
    // Available height: 688 - 16 = 672.
    s.set_window_height(Some(&3), SizeChange::SetProportion(25.));
    s.verify_invariants();
    let h3 = s.window_rect(&3).unwrap().size.h;
    assert!((h3 - 168.).abs() <= 1., "{h3}");
    // Width of 1 is untouched.
    assert_eq!(s.window_rect(&1).unwrap().size.w, 616.);

    s.reset_window_height(Some(&3));
    let h3 = s.window_rect(&3).unwrap().size.h;
    assert!((h3 - 336.).abs() <= 1., "{h3}");
}

#[test]
fn interactive_resize_moves_one_divider() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    let w3 = s.window_rect(&3).unwrap().size.w;
    let w2 = s.window_rect(&2).unwrap().size.w;
    let w1_0 = s.window_rect(&1).unwrap().size.w;

    assert!(s.interactive_resize_begin(1, ResizeEdge::RIGHT));
    assert!(s.interactive_resize_update(&1, Point::from((100., 0.))));
    s.verify_invariants();
    let w1 = s.window_rect(&1).unwrap().size.w;
    assert!((w1 - (w1_0 + 100.)).abs() <= 1., "{w1}");
    assert!((s.window_rect(&2).unwrap().size.w - (w2 - 100.)).abs() <= 1.);
    // Only the two siblings around the divider change.
    assert_eq!(s.window_rect(&3).unwrap().size.w, w3);

    // Updates are relative to the start of the resize.
    assert!(s.interactive_resize_update(&1, Point::from((50., 0.))));
    let w1 = s.window_rect(&1).unwrap().size.w;
    assert!((w1 - (w1_0 + 50.)).abs() <= 1., "{w1}");

    s.interactive_resize_end(Some(&1));
    s.verify_invariants();

    // Left edge of the middle window moves the divider with window 1.
    assert!(s.interactive_resize_begin(2, ResizeEdge::LEFT));
    let w1 = s.window_rect(&1).unwrap().size.w;
    assert!(s.interactive_resize_update(&2, Point::from((-30., 0.))));
    assert!((s.window_rect(&1).unwrap().size.w - (w1 - 30.)).abs() <= 1.);
    assert_eq!(s.window_rect(&3).unwrap().size.w, w3);
    s.interactive_resize_end(None);
    s.verify_invariants();
}

#[test]
fn insert_position_sides() {
    let mut s = space();

    // Empty tree: the root.
    assert!(matches!(
        s.insert_position(Point::from((640., 360.))),
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Root,
            ..
        })
    ));

    add(&mut s, 1);
    add(&mut s, 2);
    let leaf1 = s.leaf_id_of(&1).unwrap();
    let leaf2 = s.leaf_id_of(&2).unwrap();
    // 1 spans x 16..632, 2 spans x 648..1264, both y 16..704.

    let at = |x: f64, y: f64| s.insert_position(Point::from((x, y)));
    let docked = |target, side| {
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Leaf(target),
            side,
        })
    };

    assert_eq!(at(100., 360.), docked(leaf1, DockSide::Left));
    assert_eq!(at(600., 360.), docked(leaf1, DockSide::Right));
    assert_eq!(at(320., 100.), docked(leaf1, DockSide::Top));
    assert_eq!(at(320., 650.), docked(leaf1, DockSide::Bottom));
    assert_eq!(at(700., 360.), docked(leaf2, DockSide::Left));
    // Middle of a leaf: nearest side until the pointer rests there.
    assert_eq!(at(950., 300.), docked(leaf2, DockSide::Top));

    // Near the working area edge: dock beside everything.
    let root = |side| {
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Root,
            side,
        })
    };
    assert_eq!(at(10., 360.), root(DockSide::Left));
    assert_eq!(at(640., 715.), root(DockSide::Bottom));

    // Hint areas.
    // Top of leaf 1: splits that leaf vertically.
    let hint = s.insert_hint_area(at(320., 100.)).unwrap();
    let r1 = s.window_rect(&1).unwrap();
    assert_eq!(hint.loc, r1.loc);
    assert_eq!(hint.size.w, r1.size.w);
    assert!((hint.size.h - (r1.size.h - 16.) / 2.).abs() <= 1.);

    // Left of leaf 1: joins the horizontal root split, taking half of leaf 1's share.
    let hint = s.insert_hint_area(at(100., 360.)).unwrap();
    assert_eq!(hint.loc, r1.loc);
    assert!((hint.size.w - (1248. - 32.) / 4.).abs() <= 1.);

    // Root bottom: two rows over the full width.
    let hint = s.insert_hint_area(root(DockSide::Bottom)).unwrap();
    assert_eq!(hint.size.w, 1248.);
    assert!((hint.loc.y - (16. + 336. + 16.)).abs() <= 1.);

    // Root left with an existing horizontal root: joins it as a third column.
    let hint = s.insert_hint_area(root(DockSide::Left)).unwrap();
    assert_eq!(hint.loc.x, 16.);
    assert!((hint.size.w - (1248. - 32.) / 3.).abs() <= 1.);

    assert_eq!(s.insert_hint_area(InsertPosition::Floating), None);
}

#[test]
fn docking_at_root_edge() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    let tile3 = tile(&s, 3);
    s.add_tile_docked(
        tile3,
        DockInsert {
            target: DockTarget::Root,
            side: DockSide::Bottom,
        },
        true,
    );
    s.verify_invariants();
    assert_eq!(s.tree_string(), "V[H[1,2],3]");

    let tile4 = tile(&s, 4);
    s.add_tile_docked(
        tile4,
        DockInsert {
            target: DockTarget::Root,
            side: DockSide::Top,
        },
        false,
    );
    s.verify_invariants();
    assert_eq!(s.tree_string(), "V[4,H[1,2],3]");
    assert_eq!(active(&s), 3);
}

#[test]
fn fullscreen_and_maximize_request_sizes() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);

    assert!(s.set_fullscreen(&1, true));
    s.verify_invariants();
    let win = s.tiles().find(|t| *t.window().id() == 1).unwrap().window();
    assert_eq!(win.pending_sizing_mode(), SizingMode::Fullscreen);
    assert_eq!(win.requested_size(), Some(Size::from((1280, 720))));

    // The other window keeps its slot.
    let win2 = s.tiles().find(|t| *t.window().id() == 2).unwrap().window();
    assert_eq!(win2.pending_sizing_mode(), SizingMode::Normal);
    assert_eq!(win2.requested_size(), Some(Size::from((616, 688))));

    assert!(s.set_maximized(&2, true));
    s.verify_invariants();
    let win2 = s.tiles().find(|t| *t.window().id() == 2).unwrap().window();
    assert_eq!(win2.pending_sizing_mode(), SizingMode::Maximized);
    assert_eq!(win2.requested_size(), Some(Size::from((1280, 720))));

    assert!(s.set_fullscreen(&1, false));
    assert!(!s.set_fullscreen(&1, false));
    s.verify_invariants();
    let win = s.tiles().find(|t| *t.window().id() == 1).unwrap().window();
    assert_eq!(win.requested_size(), Some(Size::from((616, 688))));
}

#[test]
fn column_round_trip() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    let column = s.remove_active_column().unwrap();
    s.verify_invariants();
    assert_eq!(s.tree_string(), "1");

    s.add_column(None, column, true, None);
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[1,2]");
    assert_eq!(active(&s), 2);
}

#[test]
fn layout_ops_drag_to_dock() {
    let ops = [
        Op::AddOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
        Op::AddWindow {
            params: TestWindowParams::new(2),
        },
        Op::InteractiveMoveBegin {
            window: 2,
            output_idx: 1,
            px: 900.,
            py: 360.,
        },
        Op::InteractiveMoveUpdate {
            window: 2,
            dx: 0.,
            dy: 0.,
            output_idx: 1,
            px: 900.,
            py: 360.,
        },
        // Drag over the bottom part of window 1.
        Op::InteractiveMoveUpdate {
            window: 2,
            dx: -600.,
            dy: 300.,
            output_idx: 1,
            px: 300.,
            py: 650.,
        },
        Op::InteractiveMoveEnd { window: 2 },
    ];

    let layout = check_ops(ops);
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "V[1,2]");
}

#[test]
fn consume_and_expel() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    assert_eq!(active(&s), 3);

    // Stack 3 below its left neighbor.
    s.consume_or_expel_window_left(None);
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[1,V[2,3]]");
    assert_eq!(active(&s), 3);

    // Already stacked: expel to the right of the stack.
    s.consume_or_expel_window_right(None);
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[1,2,3]");

    // Consume the right neighbor of the active window below it.
    s.activate_window(&1);
    s.consume_into_column();
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[V[1,2],3]");
    assert_eq!(active(&s), 1);

    s.expel_from_column();
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[2,1,3]");

    // Expel from a vertical root wraps it in a horizontal split.
    let mut s = space();
    add(&mut s, 1);
    dock(&mut s, 2, 1, DockSide::Bottom);
    dock(&mut s, 3, 2, DockSide::Bottom);
    assert_eq!(s.tree_string(), "V[1,2,3]");
    s.consume_or_expel_window_left(None);
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[3,V[1,2]]");
}

#[test]
fn toggle_floating_docks_right_of_last_docked() {
    let mut floating = TestWindowParams::new(3);
    floating.is_floating = true;
    let ops = [
        Op::AddOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
        Op::AddWindow {
            params: TestWindowParams::new(2),
        },
        Op::FocusWindow(1),
        Op::AddWindow { params: floating },
        // The floating window is focused; docking it goes right of 1, the last docked focus.
        Op::ToggleWindowFloating { id: None },
    ];

    let mut layout = check_ops(ops);
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "H[1,3,2]");
    assert!(!ws.is_floating(&3));

    // And back out to the Desktop.
    check_ops_on_layout(
        &mut layout,
        [Op::SetWindowFloating {
            id: None,
            floating: true,
        }],
    );
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "H[1,2]");
    assert!(ws.is_floating(&3));
}

fn floating_drag(window: usize, from: (f64, f64), to: (f64, f64)) -> [Op; 4] {
    [
        Op::InteractiveMoveBegin {
            window,
            output_idx: 1,
            px: from.0,
            py: from.1,
        },
        Op::InteractiveMoveUpdate {
            window,
            dx: 0.,
            dy: 0.,
            output_idx: 1,
            px: from.0,
            py: from.1,
        },
        Op::InteractiveMoveUpdate {
            window,
            dx: to.0 - from.0,
            dy: to.1 - from.1,
            output_idx: 1,
            px: to.0,
            py: to.1,
        },
        Op::InteractiveMoveEnd { window },
    ]
}

#[test]
fn floating_drag_docks_only_on_dock_zones() {
    let mut p2 = TestWindowParams::new(2);
    p2.is_floating = true;
    let mut p3 = TestWindowParams::new(3);
    p3.is_floating = true;
    let mut layout = check_ops([
        Op::AddOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
        Op::AddWindow { params: p2 },
        Op::AddWindow { params: p3 },
    ]);

    // Dropping a floating window in the middle of a docked window keeps it floating.
    check_ops_on_layout(&mut layout, floating_drag(3, (640., 360.), (600., 300.)));
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "1");
    assert!(ws.is_floating(&3));

    // Dropping at the screen edge docks beside everything.
    check_ops_on_layout(&mut layout, floating_drag(2, (640., 360.), (10., 360.)));
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "H[2,1]");
    assert!(!ws.is_floating(&2));

    // Dropping on the bottom edge band of a docked window docks below it.
    let r1 = ws.scrolling().window_rect(&1).unwrap();
    let to = (r1.loc.x + r1.size.w / 2., r1.loc.y + r1.size.h - 60.);
    check_ops_on_layout(&mut layout, floating_drag(3, (640., 360.), to));
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "H[2,V[1,3]]");
}

#[test]
fn insert_and_remove_preserve_fractions() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    // Make 1 wider: [0.5, 0.25, 0.25] -> widen 1 by 100px.
    s.set_window_width(Some(&1), SizeChange::AdjustFixed(100));
    s.verify_invariants();
    let w = |s: &DockSpace<TestWindow>, id: usize| s.window_rect(&id).unwrap().size.w;
    let (w1, w2, w3) = (w(&s, 1), w(&s, 2), w(&s, 3));

    // Docking right of 3 splits only 3's share. (Everyone shrinks a little for the new gap.)
    dock(&mut s, 4, 3, DockSide::Right);
    assert_eq!(s.tree_string(), "H[1,2,3,4]");
    let shrink = (1216. - 16.) / 1216.;
    assert!((w(&s, 1) - w1 * shrink).abs() <= 1.);
    assert!((w(&s, 2) - w2 * shrink).abs() <= 1.);
    assert!((w(&s, 3) - w(&s, 4)).abs() <= 1.);
    assert!((w(&s, 3) + w(&s, 4) - w3 * shrink).abs() <= 1.);

    // Removing 4 gives its space back to its only neighbor, 3.
    remove(&mut s, 4);
    assert_eq!(w(&s, 1), w1);
    assert_eq!(w(&s, 2), w2);
    assert!((w(&s, 3) - w3).abs() <= 1.);

    // Removing a middle child splits its space between both neighbors proportionally.
    remove(&mut s, 2);
    let total = w1 + w3;
    let freed = w2 + 16.;
    assert!((w(&s, 1) - (w1 + freed * w1 / total)).abs() <= 1.5);
    assert!((w(&s, 3) - (w3 + freed * w3 / total)).abs() <= 1.5);

    // Docking at the root edge takes an equal share proportionally from everyone.
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    let leaf = DockInsert {
        target: DockTarget::Root,
        side: DockSide::Left,
    };
    let t = tile(&s, 3);
    s.add_tile_docked(t, leaf, true);
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[3,1,2]");
    let ws: Vec<f64> = (1..=3).map(|id| w(&s, id)).collect();
    assert!(ws.iter().all(|x| (x - ws[0]).abs() <= 1.), "{ws:?}");
}

#[test]
fn divider_under_and_drag() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    dock(&mut s, 3, 2, DockSide::Bottom);
    assert_eq!(s.tree_string(), "H[1,V[2,3]]");

    // Inside windows: no divider.
    assert_eq!(s.divider_under(Point::from((300., 300.))), None);

    // The vertical gap between 1 and the stack.
    let r1 = s.window_rect(&1).unwrap();
    let gap_x = r1.loc.x + r1.size.w + 8.;
    assert_eq!(
        s.divider_under(Point::from((gap_x, 300.))),
        Some((1, ResizeEdge::RIGHT))
    );

    // The horizontal gap between 2 and 3.
    let r2 = s.window_rect(&2).unwrap();
    let gap_y = r2.loc.y + r2.size.h + 8.;
    assert_eq!(
        s.divider_under(Point::from((r2.loc.x + 10., gap_y))),
        Some((2, ResizeEdge::BOTTOM))
    );
    // The same y left of the stack is inside window 1.
    assert_eq!(s.divider_under(Point::from((300., gap_y))), None);

    // Dragging the vertical divider resizes 1 and the stack only.
    let (win, edge) = s.divider_under(Point::from((gap_x, 300.))).unwrap();
    let w1 = r1.size.w;
    let w2 = r2.size.w;
    assert!(s.interactive_resize_begin(win, edge));
    assert!(s.interactive_resize_update(&1, Point::from((-100., 0.))));
    s.interactive_resize_end(None);
    s.verify_invariants();
    assert_eq!(s.window_rect(&1).unwrap().size.w, w1 - 100.);
    assert_eq!(s.window_rect(&2).unwrap().size.w, w2 + 100.);
    assert_eq!(s.window_rect(&3).unwrap().size.w, w2 + 100.);
    // Heights in the stack are unchanged.
    assert_eq!(s.window_rect(&2).unwrap().size.h, r2.size.h);
}

#[test]
fn divider_of_nested_branch_resizes_that_branch() {
    let mut s = space();
    add(&mut s, 1);
    dock(&mut s, 2, 1, DockSide::Left);
    dock(&mut s, 3, 2, DockSide::Bottom);
    dock(&mut s, 4, 2, DockSide::Right);
    assert_eq!(s.tree_string(), "H[V[H[2,4],3],1]");

    let r1 = s.window_rect(&1).unwrap();
    let r2 = s.window_rect(&2).unwrap();
    let r4 = s.window_rect(&4).unwrap();
    let r3 = s.window_rect(&3).unwrap();
    let pos = Point::from((r1.loc.x - 8., r1.loc.y + 10.));
    let (win, edge) = s.divider_under(pos).unwrap();
    assert_eq!(edge, ResizeEdge::RIGHT);
    assert!(win == 3 || win == 4, "{win}");

    assert!(s.interactive_resize_begin(win, edge));
    assert!(s.interactive_resize_update(&win, Point::from((50., 0.))));
    s.interactive_resize_end(None);
    s.verify_invariants();
    assert_eq!(s.window_rect(&1).unwrap().size.w, r1.size.w - 50.);
    // The whole left branch grows.
    assert_eq!(s.window_rect(&3).unwrap().size.w, r3.size.w + 50.);
    let w24 = s.window_rect(&2).unwrap().size.w + s.window_rect(&4).unwrap().size.w;
    assert!((w24 - (r2.size.w + r4.size.w + 50.)).abs() <= 1.);
}

#[test]
fn divider_band_with_zero_gaps() {
    let view_size = Size::from((1280., 720.));
    let mut options = Options::default();
    options.layout.gaps = 0.;
    let mut s = DockSpace::new(
        view_size,
        Rectangle::from_size(view_size),
        1.,
        Clock::with_time(Duration::ZERO),
        Rc::new(options),
    );
    add(&mut s, 1);
    add(&mut s, 2);
    assert_eq!(s.window_rect(&1).unwrap().size.w, 640.);

    assert_eq!(
        s.divider_under(Point::from((638., 300.))),
        Some((1, ResizeEdge::RIGHT))
    );
    assert_eq!(
        s.divider_under(Point::from((642., 300.))),
        Some((1, ResizeEdge::RIGHT))
    );
    assert_eq!(s.divider_under(Point::from((630., 300.))), None);
}

#[test]
fn minimize_keeps_slot() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    s.set_window_width(Some(&1), SizeChange::AdjustFixed(100));
    let rects: Vec<_> = (1..=3).map(|id| s.window_rect(&id).unwrap()).collect();
    s.activate_window(&2);

    assert!(s.minimize_window(&2));
    s.verify_invariants();
    assert!(s.is_minimized(&2));
    assert_eq!(s.tree_string(), "H[1,2,3]");
    // The active window moved to a visible neighbor.
    assert!(matches!(active(&s), 1 | 3));
    // Siblings take the space: 1 and 3 now fill the whole width.
    let r1 = s.window_rect(&1).unwrap();
    let r3 = s.window_rect(&3).unwrap();
    assert_eq!(r1.loc.x, 16.);
    assert_eq!(r3.loc.x + r3.size.w, 1264.);
    assert_eq!(r3.loc.x, r1.loc.x + r1.size.w + 16.);
    // The minimized window is hidden.
    assert!(s
        .tiles_with_render_positions()
        .any(|(tile, _, visible)| *tile.window().id() == 2 && !visible));
    for x in (20..1260).step_by(40) {
        if let Some((win, _)) = s.window_under(Point::from((f64::from(x), 360.))) {
            assert_ne!(*win.id(), 2);
        }
    }

    // Docking next to 1 while 2 is minimized works and keeps 2's slot.
    dock(&mut s, 4, 1, DockSide::Bottom);
    assert_eq!(s.tree_string(), "H[V[1,4],2,3]");
    remove(&mut s, 4);

    // Activating restores it into the same slot, same size.
    assert!(s.activate_window(&2));
    s.verify_invariants();
    assert!(!s.is_minimized(&2));
    assert_eq!(active(&s), 2);
    for (id, rect) in (1..=3).zip(&rects) {
        assert_eq!(s.window_rect(&id).unwrap(), *rect, "window {id}");
    }
}

#[test]
fn minimize_everything() {
    let mut s = space();
    add(&mut s, 1);
    dock(&mut s, 2, 1, DockSide::Bottom);
    assert!(s.minimize_window(&1));
    assert!(s.minimize_window(&2));
    s.verify_invariants();
    assert!(s.is_empty());
    assert!(s.active_window().is_none());
    assert_eq!(s.last_minimized_window().map(|(_, id)| *id), Some(2));

    // A new window docks with the minimized ones still in the tree.
    add(&mut s, 3);
    assert_eq!(active(&s), 3);
    assert!(!s.is_empty());

    assert!(s.restore_window(&2));
    s.verify_invariants();
    assert_eq!(s.last_minimized_window().map(|(_, id)| *id), Some(1));
}

#[test]
fn minimize_and_restore_through_layout() {
    let mut floating = TestWindowParams::new(3);
    floating.is_floating = true;
    let mut layout = check_ops([
        Op::AddOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
        Op::AddWindow { params: floating },
        Op::FocusWindow(1),
        Op::MinimizeWindow { id: None },
    ]);
    let ws = layout.active_workspace().unwrap();
    assert!(ws.is_minimized(&1));
    // Nothing docked is shown: the Desktop gets focus.
    assert_eq!(ws.active_window().map(|w| *w.id()), Some(3));

    // Floating windows minimize too, keeping their position.
    let pos = |layout: &Layout<TestWindow>| {
        let ws = layout.active_workspace().unwrap();
        ws.tiles_with_render_positions()
            .find(|(t, _, _)| *t.window().id() == 3)
            .map(|(_, pos, visible)| (pos, visible))
            .unwrap()
    };
    let (pos3, _) = pos(&layout);
    check_ops_on_layout(&mut layout, [Op::MinimizeWindow { id: Some(3) }]);
    assert!(layout.is_minimized(&3));
    assert!(!pos(&layout).1);
    let ws = layout.active_workspace().unwrap();
    assert!(ws.active_window().is_none());
    // The floating window was minimized last, so it is restored first.
    check_ops_on_layout(&mut layout, [Op::RestoreWindow { id: None }]);
    assert!(!layout.is_minimized(&3));
    assert_eq!(pos(&layout), (pos3, true));
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.active_window().map(|w| *w.id()), Some(3));

    check_ops_on_layout(&mut layout, [Op::RestoreWindow { id: None }]);
    let ws = layout.active_workspace().unwrap();
    assert!(!ws.is_minimized(&1));
    assert_eq!(ws.active_window().map(|w| *w.id()), Some(1));

    // Focusing a minimized window restores it.
    check_ops_on_layout(
        &mut layout,
        [Op::MinimizeWindow { id: Some(1) }, Op::FocusWindow(1)],
    );
    assert!(!layout.is_minimized(&1));
}

fn tab(space: &mut DockSpace<TestWindow>, id: usize, target: usize) {
    let leaf = space.leaf_id_of(&target).unwrap();
    let tile = tile(space, id);
    let insert = DockInsert {
        target: DockTarget::Tab(leaf),
        side: DockSide::Right,
    };
    space.add_tile_docked(tile, insert, true);
    space.verify_invariants();
}

#[test]
fn tab_groups() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    let r2 = s.window_rect(&2).unwrap();

    tab(&mut s, 3, 2);
    assert_eq!(s.tree_string(), "H[1,T[2,*3]]");
    assert_eq!(active(&s), 3);
    // The group keeps the slot; both tabs get its size.
    assert_eq!(s.window_rect(&3).unwrap(), r2);
    let win = |s: &DockSpace<TestWindow>, id: usize| {
        s.tiles()
            .find(|t| *t.window().id() == id)
            .unwrap()
            .window()
            .requested_size()
    };
    assert_eq!(win(&s, 2), win(&s, 3));

    // Only the shown tab is visible.
    let visible: Vec<usize> = s
        .tiles_with_render_positions()
        .filter(|(_, _, v)| *v)
        .map(|(t, _, _)| *t.window().id())
        .collect();
    assert!(visible.contains(&3) && !visible.contains(&2), "{visible:?}");

    // Cycling tabs.
    assert!(s.focus_tab(true));
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[1,T[*2,3]]");
    assert_eq!(active(&s), 2);
    assert!(s.focus_tab(false));
    assert_eq!(s.tree_string(), "H[1,T[2,*3]]");

    // Activating a hidden tab shows it.
    s.activate_window(&1);
    s.activate_window(&2);
    assert_eq!(s.tree_string(), "H[1,T[*2,3]]");
    s.verify_invariants();

    // Fullscreen applies to the shown tab; going fullscreen from a hidden tab shows it.
    assert!(s.set_fullscreen(&3, true));
    s.verify_invariants();
    assert_eq!(s.tree_string(), "H[1,T[2,*3]]");
    assert!(!s.set_fullscreen(&2, false));
    // Switching tabs drops fullscreen.
    s.activate_window(&2);
    s.verify_invariants();

    // Removing a tab (e.g. tearing it off) keeps the leaf.
    remove(&mut s, 2);
    assert_eq!(s.tree_string(), "H[1,3]");
    assert_eq!(active(&s), 3);
    assert_eq!(s.window_rect(&3).unwrap(), r2);
}

#[test]
fn tab_center_zone_and_hint() {
    let mut s = space();
    add(&mut s, 1);
    add(&mut s, 2);
    let leaf2 = s.leaf_id_of(&2).unwrap();
    let r2 = s.window_rect(&2).unwrap();

    let center = rect_center_of(r2);
    // Not right away: the pointer has to rest in the middle first.
    assert!(matches!(
        s.insert_position(center),
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Leaf(_),
            ..
        })
    ));
    advance_clock(&s, 200);
    assert!(matches!(
        s.insert_position(center),
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Leaf(_),
            ..
        })
    ));
    advance_clock(&s, 250);
    let pos = s.insert_position(center);
    assert_eq!(
        pos,
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Tab(leaf2),
            side: DockSide::Right,
        })
    );
    // The hint shows the whole target.
    assert_eq!(s.insert_hint_area(pos), Some(r2));

    // Outside the middle: dock beside.
    let near_top = Point::from((center.x, r2.loc.y + 60.));
    assert!(matches!(
        s.insert_position(near_top),
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Leaf(_),
            side: DockSide::Top,
        })
    ));
}

fn advance_clock(space: &DockSpace<TestWindow>, ms: u64) {
    let mut clock = space.clock().clone();
    let now = clock.now_unadjusted();
    clock.set_unadjusted(now + Duration::from_millis(ms));
}

fn rect_center_of(r: Rectangle<f64, Logical>) -> Point<f64, Logical> {
    Point::from((r.loc.x + r.size.w / 2., r.loc.y + r.size.h / 2.))
}

#[test]
fn drag_to_center_creates_tab_group_and_tear_off() {
    let ops = [
        Op::AddOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
        Op::AddWindow {
            params: TestWindowParams::new(2),
        },
        Op::InteractiveMoveBegin {
            window: 2,
            output_idx: 1,
            px: 900.,
            py: 360.,
        },
        Op::InteractiveMoveUpdate {
            window: 2,
            dx: 0.,
            dy: 0.,
            output_idx: 1,
            px: 900.,
            py: 360.,
        },
        // Drag into the middle of window 1 and rest there.
        Op::InteractiveMoveUpdate {
            window: 2,
            dx: -260.,
            dy: 0.,
            output_idx: 1,
            px: 640.,
            py: 360.,
        },
    ];

    let mut layout = check_ops(ops);
    // Frames keep coming (and update the insert hint) while the pointer rests.
    for _ in 0..5 {
        layout.update_render_elements(None);
        check_ops_on_layout(&mut layout, [Op::AdvanceAnimations { msec_delta: 100 }]);
    }
    check_ops_on_layout(&mut layout, [Op::InteractiveMoveEnd { window: 2 }]);
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "T[1,*2]");

    // Tear tab 1 off to the right side of the screen.
    check_ops_on_layout(
        &mut layout,
        [
            Op::InteractiveMoveBegin {
                window: 1,
                output_idx: 1,
                px: 640.,
                py: 360.,
            },
            Op::InteractiveMoveUpdate {
                window: 1,
                dx: 0.,
                dy: 0.,
                output_idx: 1,
                px: 640.,
                py: 360.,
            },
            Op::InteractiveMoveUpdate {
                window: 1,
                dx: 630.,
                dy: 0.,
                output_idx: 1,
                px: 1270.,
                py: 360.,
            },
            Op::InteractiveMoveEnd { window: 1 },
        ],
    );
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "H[2,1]");
}

#[test]
fn title_bar_size_accounting_and_hits() {
    use crate::layout::title_bar::TitleBarHit;

    let mut s = space();
    let mut params = TestWindowParams::new(1);
    params.has_ssd = true;
    let window = TestWindow::new(params);
    let t = Tile::new(
        window,
        Size::from((1280., 720.)),
        1.,
        s.clock().clone(),
        s.options().clone(),
    );
    s.add_tile(None, t, true, ColumnWidth::Proportion(0.5), false, None);
    s.verify_invariants();
    add(&mut s, 2);

    let r1 = s.window_rect(&1).unwrap();
    let r2 = s.window_rect(&2).unwrap();
    let requested = |s: &DockSpace<TestWindow>, id: usize| {
        s.tiles()
            .find(|t| *t.window().id() == id)
            .unwrap()
            .window()
            .requested_size()
            .unwrap()
    };
    // The window gets the slot height minus the 28 px bar; no bar for the CSD window.
    assert_eq!(requested(&s, 1).h, r1.size.h as i32 - 28);
    assert_eq!(requested(&s, 2).h, r2.size.h as i32);

    let tile = s.tiles().find(|t| *t.window().id() == 1).unwrap();
    assert_eq!(tile.title_bar_height(), 28.);
    assert_eq!(tile.tile_height_for_window_height(100.), 128.);
    assert_eq!(tile.window_height_for_tile_height(128.), 100.);
    let tile2 = s.tiles().find(|t| *t.window().id() == 2).unwrap();
    assert_eq!(tile2.title_bar_height(), 0.);
    assert_eq!(tile2.title_bar_hit(Point::from((16., 14.))), None);

    // Hit testing is relative to the tile.
    assert_eq!(
        tile.title_bar_hit(Point::from((16., 14.))),
        Some(TitleBarHit::Close)
    );
    assert_eq!(
        tile.title_bar_hit(Point::from((80., 14.))),
        Some(TitleBarHit::Bar)
    );
    assert_eq!(tile.title_bar_hit(Point::from((80., 40.))), None);
}

#[test]
fn title_bars_off_and_rule() {
    let mut options = Options::default();
    options.layout.title_bars.off = true;
    let options = Rc::new(options);
    let mut params = TestWindowParams::new(1);
    params.has_ssd = true;
    let tile = Tile::new(
        TestWindow::new(params.clone()),
        Size::from((1280., 720.)),
        1.,
        Clock::with_time(Duration::ZERO),
        options,
    );
    assert_eq!(tile.title_bar_height(), 0.);

    params.rules = Some(ResolvedWindowRules {
        title_bar: Some(false),
        ..Default::default()
    });
    let tile = Tile::new(
        TestWindow::new(params),
        Size::from((1280., 720.)),
        1.,
        Clock::with_time(Duration::ZERO),
        Rc::new(Options::default()),
    );
    assert_eq!(tile.title_bar_height(), 0.);
}

#[test]
fn floating_drag_rests_into_tab_group() {
    let mut p2 = TestWindowParams::new(2);
    p2.is_floating = true;
    let mut layout = check_ops([
        Op::AddOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
        Op::AddWindow { params: p2 },
    ]);

    // Passing over the middle without resting keeps the window floating.
    check_ops_on_layout(&mut layout, floating_drag(2, (900., 500.), (640., 360.)));
    let ws = layout.active_workspace().unwrap();
    assert!(ws.is_floating(&2));

    // Resting there joins the tab group.
    let [begin, update0, update1, end] = floating_drag(2, (900., 500.), (640., 360.));
    check_ops_on_layout(
        &mut layout,
        [
            begin,
            update0,
            update1.clone(),
            Op::AdvanceAnimations { msec_delta: 450 },
            update1,
            end,
        ],
    );
    let ws = layout.active_workspace().unwrap();
    assert_eq!(ws.scrolling().tree_string(), "T[1,*2]");
}

fn camera_space() -> DockSpace<TestWindow> {
    let s = space_with(Options::default());
    let mut clock = s.clock().clone();
    clock.set_complete_instantly(true);
    s
}

fn view_rect_of(s: &DockSpace<TestWindow>, id: usize) -> Rectangle<f64, Logical> {
    s.camera_view().rect_to_view(s.window_rect(&id).unwrap())
}

#[test]
fn canvas_grows_and_camera_follows_focus() {
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    assert_eq!(s.tree_string(), "H[1,2,3]");
    // Three 480 px minimum leaves need a 1472 px wide canvas: they all get their minimum
    // (the fractions [0.5, 0.25, 0.25] would leave 2 and 3 too small).
    for id in 1..=3 {
        assert_eq!(s.window_rect(&id).unwrap().size.w, 480.);
    }
    let r3 = s.window_rect(&3).unwrap();
    assert!(r3.loc.x + r3.size.w > 1280.);

    // Window 3 is focused: the camera pans so that it is fully in view, at actual size.
    s.advance_animations();
    let cam = s.camera_view();
    assert_eq!(cam.zoom, 1.);
    let v3 = view_rect_of(&s, 3);
    assert!(v3.loc.x >= 0. && v3.loc.x + v3.size.w <= 1280., "{v3:?}");

    // Focusing the first window pans back.
    s.focus_column_first();
    s.advance_animations();
    assert_eq!(s.camera_view().offset.x, 0.);
    let v1 = view_rect_of(&s, 1);
    assert_eq!(v1.loc.x, 16.);
}

#[test]
fn camera_show_all_frame_and_history() {
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    s.advance_animations();
    let follow = s.camera_view();

    s.camera_show_all();
    s.advance_animations();
    let all = s.camera_view();
    assert!(all.zoom < 1.);
    // Everything is in view.
    for id in 1..=3 {
        let r = view_rect_of(&s, id);
        assert!(
            r.loc.x >= 0. && r.loc.x + r.size.w <= 1280.01,
            "{id}: {r:?}"
        );
        assert!(r.loc.y >= 0. && r.loc.y + r.size.h <= 720.01, "{id}: {r:?}");
    }
    assert!(s.is_zoomed());

    // Zoomed out, windows only take activation clicks.
    let r1 = view_rect_of(&s, 1);
    // (Test windows stay 100x200 until they get configured, so aim near the corner.)
    let point = r1.loc + Point::from((5., 5.));
    let (win, hit) = s.window_under(point).unwrap();
    assert_eq!(*win.id(), 1);
    assert!(matches!(hit, HitType::Activate { .. }));

    // Framing a window shows it at actual size (it fits), centered in the working area.
    s.camera_frame_window(Some(&1));
    s.advance_animations();
    assert_eq!(s.camera_view().zoom, 1.);
    assert_eq!(active(&s), 1);
    let r1 = view_rect_of(&s, 1);
    assert!((r1.loc.x + r1.size.w / 2. - 640.).abs() <= 1., "{r1:?}");

    // Back goes to show-all, back again to following, forward returns.
    s.camera_back();
    s.advance_animations();
    assert_eq!(s.camera_view(), all);
    s.camera_back();
    s.advance_animations();
    assert_eq!(s.camera_view(), follow);
    s.camera_forward();
    s.advance_animations();
    assert_eq!(s.camera_view(), all);
}

#[test]
fn camera_zoom_levels() {
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    dock(&mut s, 3, 2, DockSide::Bottom);
    dock(&mut s, 4, 1, DockSide::Right);
    assert_eq!(s.tree_string(), "H[1,4,V[2,3]]");
    s.activate_window(&3);
    s.advance_animations();

    // Zooming out frames the parent (the stack of 2 and 3); it fits, so the next level up.
    s.camera_zoom_out();
    s.advance_animations();
    let level = s.camera_view();
    for id in 1..=4 {
        let r = view_rect_of(&s, id);
        assert!(
            r.loc.x >= -0.01 && r.loc.x + r.size.w <= 1280.01,
            "{id}: {r:?}"
        );
    }
    assert!(level.zoom < 1.);

    // Zooming out at the root does nothing more than show everything.
    s.camera_zoom_out();
    s.advance_animations();
    assert!((s.camera_view().zoom - level.zoom).abs() < 1e-9);

    // Zooming in walks back down toward the focused window and finally to actual size.
    for _ in 0..4 {
        s.camera_zoom_in();
        s.advance_animations();
    }
    assert_eq!(s.camera_view().zoom, 1.);
    let r3 = view_rect_of(&s, 3);
    assert!(r3.loc.x >= 0. && r3.loc.x + r3.size.w <= 1280.);
}

#[test]
fn camera_pan_moves_focus_and_view() {
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    add(&mut s, 3);
    s.focus_column_first();
    s.advance_animations();
    assert_eq!(s.camera_view().offset.x, 0.);

    s.camera_pan(DockSide::Right);
    s.camera_pan(DockSide::Right);
    s.advance_animations();
    assert_eq!(active(&s), 3);
    let v3 = view_rect_of(&s, 3);
    assert!(v3.loc.x + v3.size.w <= 1280.);
    s.camera_back();
    s.camera_back();
    s.advance_animations();
    assert_eq!(s.camera_view().offset.x, 0.);
}

#[test]
fn camera_zoom_maps_input_and_hints() {
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    s.camera_zoom_by(0.5, Point::from((0., 0.)));
    let cam = s.camera_view();
    assert_eq!(cam.zoom, 0.5);

    // The divider between 1 and 2, seen at half size.
    let r1 = s.window_rect(&1).unwrap();
    let gap_x = cam.to_view(Point::from((r1.loc.x + r1.size.w + 8., 300.)));
    assert_eq!(s.divider_under(gap_x), Some((1, ResizeEdge::RIGHT)));

    // Hints come back in view coordinates.
    let leaf2 = s.leaf_id_of(&2).unwrap();
    let hint = s
        .insert_hint_area(InsertPosition::Dock(DockInsert {
            target: DockTarget::Tab(leaf2),
            side: DockSide::Right,
        }))
        .unwrap();
    assert_eq!(hint, cam.rect_to_view(s.window_rect(&2).unwrap()));

    // Dragging the divider by 50 view pixels moves it 100 canvas pixels.
    let w1 = r1.size.w;
    assert!(s.interactive_resize_begin(1, ResizeEdge::RIGHT));
    assert!(s.interactive_resize_update(&1, Point::from((50., 0.))));
    s.interactive_resize_end(None);
    assert_eq!(s.window_rect(&1).unwrap().size.w, w1 + 100.);
}

#[test]
fn canvas_only_grows_when_needed() {
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    // Two leaves fit: no camera movement, same as without a minimum.
    assert_eq!(s.window_rect(&1).unwrap().size.w, 616.);
    assert!(s.camera_view().is_identity());

    // A small child takes its minimum from its sibling, without growing the canvas.
    let mut s = camera_space();
    add(&mut s, 1);
    add(&mut s, 2);
    s.set_window_width(Some(&2), SizeChange::SetFixed(100));
    let w2 = s.window_rect(&2).unwrap().size.w;
    assert_eq!(w2, 480.);
    assert_eq!(s.window_rect(&1).unwrap().size.w, 1248. - 16. - 480.);
}

#[test]
fn icon_tiles_below_threshold() {
    let make = |threshold: f64| {
        let mut options = Options::default();
        options.layout.camera.icon_threshold = threshold;
        let s = space_with(options);
        let mut clock = s.clock().clone();
        clock.set_complete_instantly(true);
        s
    };

    let build = |s: &mut DockSpace<TestWindow>| {
        add(s, 1);
        add(s, 2);
        dock(s, 3, 2, DockSide::Bottom);
        add(s, 4);
        add(s, 5);
        s.camera_show_all();
        s.advance_animations();
    };

    // At actual size nothing collapses.
    let mut s = make(160.);
    add(&mut s, 1);
    assert!(s.icon_tile_leaf_counts().is_empty());

    // Zoomed out: windows narrower than the threshold become icon tiles.
    let mut s = make(160.);
    build(&mut s);
    let zoom = s.camera_view().zoom;
    assert!(zoom < 1.);
    let counts = s.icon_tile_leaf_counts();
    let small = (1..=5)
        .filter(|id| s.window_rect(id).unwrap().size.w * zoom < 160.)
        .count();
    assert_eq!(counts.iter().sum::<usize>(), small);

    // A huge threshold collapses the whole tree into one tile.
    let mut s = make(100_000.);
    build(&mut s);
    assert_eq!(s.icon_tile_leaf_counts(), vec![5]);

    // Clicking anywhere in a collapsed window's slot activates it.
    let r4 = s.camera_view().rect_to_view(s.window_rect(&4).unwrap());
    let center = Point::from((r4.loc.x + r4.size.w / 2., r4.loc.y + r4.size.h - 1.));
    let (win, hit) = s.window_under(center).unwrap();
    assert_eq!(*win.id(), 4);
    assert!(matches!(hit, HitType::Activate { .. }));
}
