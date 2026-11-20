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

    // The active leaf's parent is already horizontal: join as a sibling, equal fractions.
    add(&mut s, 3);
    assert_eq!(s.tree_string(), "H[1,2,3]");
    let w: Vec<f64> = (1..=3)
        .map(|id| s.window_rect(&id).unwrap().size.w)
        .collect();
    assert!(w.iter().all(|x| (x - w[0]).abs() <= 1.), "{w:?}");

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

    assert!(s.interactive_resize_begin(1, ResizeEdge::RIGHT));
    assert!(s.interactive_resize_update(&1, Point::from((100., 0.))));
    s.verify_invariants();
    let w1 = s.window_rect(&1).unwrap().size.w;
    assert!((w1 - (w2 + 100.)).abs() <= 1., "{w1}");
    assert!((s.window_rect(&2).unwrap().size.w - (w2 - 100.)).abs() <= 1.);
    // Only the two siblings around the divider change.
    assert_eq!(s.window_rect(&3).unwrap().size.w, w3);

    // Updates are relative to the start of the resize.
    assert!(s.interactive_resize_update(&1, Point::from((50., 0.))));
    let w1 = s.window_rect(&1).unwrap().size.w;
    assert!((w1 - (w2 + 50.)).abs() <= 1., "{w1}");

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
    // Center of a leaf: nearest side for now.
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

    // Left of leaf 1: joins the horizontal root split as the first of three columns.
    let hint = s.insert_hint_area(at(100., 360.)).unwrap();
    assert_eq!(hint.loc, r1.loc);
    assert!((hint.size.w - (1248. - 32.) / 3.).abs() <= 1.);

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
