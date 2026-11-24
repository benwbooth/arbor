//! keep-aspect-ratio: an interactive resize of a floating window keeps the
//! proportions it had when the drag began (Grove Player's window).

use super::*;

fn requested(layout: &Layout<TestWindow>, id: usize) -> Size<i32, Logical> {
    let (_, w) = layout.windows().find(|(_, w)| *w.id() == id).unwrap();
    w.requested_size().unwrap_or_else(|| w.size())
}

fn ratio(s: Size<i32, Logical>) -> f64 {
    f64::from(s.w) / f64::from(s.h)
}

#[test]
fn floating_resize_keeps_aspect_ratio() {
    let mut params = TestWindowParams::new(1);
    params.is_floating = true;
    params.bbox = Rectangle::from_size(Size::from((800, 450)));
    params.rules = Some(ResolvedWindowRules {
        keep_aspect_ratio: Some(true),
        ..Default::default()
    });
    let mut layout = check_ops([Op::AddOutput(1), Op::AddWindow { params }, Op::Communicate(1)]);
    let start = requested(&layout, 1);
    let start_ratio = ratio(start);

    // The right edge leads; the height follows.
    check_ops_on_layout(
        &mut layout,
        [
            Op::InteractiveResizeBegin { window: 1, edges: ResizeEdge::RIGHT },
            Op::InteractiveResizeUpdate { window: 1, dx: 200., dy: 0. },
        ],
    );
    let s = requested(&layout, 1);
    assert_eq!(s.w, start.w + 200);
    assert!((ratio(s) - start_ratio).abs() < 0.01, "{s:?} from {start:?}");

    check_ops_on_layout(&mut layout, [Op::InteractiveResizeEnd { window: 1 }, Op::Communicate(1)]);

    // A corner: the side moved further (relative to its length) leads,
    // here the height.
    let start = requested(&layout, 1);
    check_ops_on_layout(
        &mut layout,
        [
            Op::InteractiveResizeBegin { window: 1, edges: ResizeEdge::BOTTOM_RIGHT },
            Op::InteractiveResizeUpdate { window: 1, dx: 10., dy: -150. },
        ],
    );
    let s = requested(&layout, 1);
    assert_eq!(s.h, start.h - 150, "{s:?} from {start:?}");
    assert!((ratio(s) - start_ratio).abs() < 0.02, "{s:?} from {start:?}");
    check_ops_on_layout(&mut layout, [Op::InteractiveResizeEnd { window: 1 }]);
}

#[test]
fn floating_resize_without_rule_is_free() {
    let mut params = TestWindowParams::new(1);
    params.is_floating = true;
    params.bbox = Rectangle::from_size(Size::from((800, 450)));
    let mut layout = check_ops([Op::AddOutput(1), Op::AddWindow { params }, Op::Communicate(1)]);
    let start = requested(&layout, 1);
    check_ops_on_layout(
        &mut layout,
        [
            Op::InteractiveResizeBegin { window: 1, edges: ResizeEdge::RIGHT },
            Op::InteractiveResizeUpdate { window: 1, dx: 200., dy: 0. },
        ],
    );
    let s = requested(&layout, 1);
    assert_eq!(s.w, start.w + 200);
    assert_eq!(s.h, start.h);
}
