//! Unit tests for the arbor dock tree (`DockSpace`).

use super::*;
use crate::layout::dock::{DockInsert, DockSide, DockSpace, DockTarget};
use crate::layout::monitor::InsertPosition;
use crate::utils::transaction::Transaction;

fn space() -> DockSpace<TestWindow> {
    let view_size = Size::from((1280., 720.));
    let options = Rc::new(Options::default());
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
    // Middle of a leaf: join its tab group.
    assert_eq!(
        at(950., 300.),
        InsertPosition::Dock(DockInsert {
            target: DockTarget::Tab(leaf2),
            side: DockSide::Right,
        })
    );

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
        // Drag into the middle of window 1.
        Op::InteractiveMoveUpdate {
            window: 2,
            dx: -260.,
            dy: 0.,
            output_idx: 1,
            px: 640.,
            py: 360.,
        },
        Op::InteractiveMoveEnd { window: 2 },
    ];

    let mut layout = check_ops(ops);
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
