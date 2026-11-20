//! Nested docking tree layout.
//!
//! The dock tree replaces niri's scrollable tiling inside a workspace. Every node is either a
//! split (children tiling the split's rectangle along one axis, with gaps between them) or a leaf
//! holding one tile. See `ARBOR.md` for the design.
//!
//! `DockSpace` deliberately exposes the same method names and signatures that `Workspace` used to
//! call on `ScrollingSpace`, so that `workspace.rs` changes stay minimal. Column-specific
//! operations are no-ops or the closest sensible equivalent.

use std::cmp::max;
use std::collections::HashMap;
use std::iter::zip;
use std::rc::Rc;
use std::time::Duration;

use niri_config::utils::MergeWith as _;
use niri_config::PresetSize;
use niri_ipc::{ColumnDisplay, SizeChange, WindowLayout};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Logical, Point, Rectangle, Scale, Serial, Size};

use super::closing_window::{ClosingWindow, ClosingWindowRenderElement};
use super::monitor::InsertPosition;
use super::scrolling::{
    compute_toplevel_bounds, compute_working_area, Column, ColumnWidth, ScrollDirection,
};
use super::tile::{Tile, TileRenderElement, TileRenderSnapshot};
use super::{ConfigureIntent, HitType, InteractiveResizeData, LayoutElement, Options, RemovedTile};
use crate::animation::{Animation, Clock};
use crate::layout::SizingMode;
use crate::niri_render_elements;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::RenderCtx;
use crate::utils::id::IdCounter;
use crate::utils::transaction::{Transaction, TransactionBlocker};
use crate::utils::ResizeEdge;
use crate::window::ResolvedWindowRules;

type Rect = Rectangle<f64, Logical>;

/// Smallest length (logical pixels) that a split child can be resized down to.
const MIN_CHILD_LEN: f64 = 50.;

/// Distance from the working area edge within which a drag docks beside everything.
const ROOT_EDGE_BAND: f64 = 40.;

/// Smallest thickness of the band around a divider that grabs it (matters when gaps are small).
const DIVIDER_HIT: f64 = 8.;

/// Largest depth of the edge band of a docked window where a window dragged from the Desktop
/// docks beside it.
const LEAF_EDGE_BAND: f64 = 80.;

static LEAF_ID_COUNTER: IdCounter = IdCounter::new();

/// Stable identity of a leaf in the dock tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeafId(u64);

impl LeafId {
    fn next() -> Self {
        Self(LEAF_ID_COUNTER.next())
    }
}

/// Axis along which a split lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side, left to right.
    Horizontal,
    /// Children stacked, top to bottom.
    Vertical,
}

/// Side of a target where a new node is docked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSide {
    Left,
    Right,
    Top,
    Bottom,
}

impl DockSide {
    fn axis(self) -> Axis {
        match self {
            DockSide::Left | DockSide::Right => Axis::Horizontal,
            DockSide::Top | DockSide::Bottom => Axis::Vertical,
        }
    }

    /// Whether the new node goes before the target along the axis.
    fn is_before(self) -> bool {
        matches!(self, DockSide::Left | DockSide::Top)
    }
}

/// What a dock operation docks against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockTarget {
    /// Beside the whole tree (or as the root of an empty tree).
    Root,
    /// Beside a particular leaf.
    Leaf(LeafId),
}

/// Where to dock a tile in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockInsert {
    pub target: DockTarget,
    pub side: DockSide,
}

niri_render_elements! {
    DockSpaceRenderElement<R> => {
        Tile = TileRenderElement<R>,
        ClosingWindow = ClosingWindowRenderElement,
    }
}

/// A leaf of the dock tree, holding one tile.
///
/// Also acts as a stand-in for a scrolling `Column` where `Workspace` iterates columns.
#[derive(Debug)]
pub struct DockLeaf<W: LayoutElement> {
    id: LeafId,
    tile: Tile<W>,
    /// Compositor-side fullscreen state (changes immediately on request).
    is_pending_fullscreen: bool,
    /// Compositor-side maximized state; can be true together with fullscreen, meaning that
    /// unfullscreening goes back to maximized.
    is_pending_maximized: bool,
    /// Layout rectangle from the last relayout.
    rect: Rect,
}

#[derive(Debug)]
struct Split<W: LayoutElement> {
    axis: Axis,
    children: Vec<Node<W>>,
    /// Fraction of the available length (excluding gaps) taken by each child. Sums to 1.
    fractions: Vec<f64>,
    /// Layout rectangle from the last relayout.
    rect: Rect,
}

#[derive(Debug)]
enum Node<W: LayoutElement> {
    Split(Split<W>),
    Leaf(Box<DockLeaf<W>>),
}

/// A divider being dragged during an interactive resize.
#[derive(Debug, Clone)]
struct DividerDrag {
    /// Path to the split that holds the divider.
    split_path: Vec<usize>,
    /// Child index of the branch containing the resized window.
    idx: usize,
    /// Child index of the neighbor on the other side of the divider.
    neighbor: usize,
    orig_frac: f64,
    orig_neighbor_frac: f64,
}

#[derive(Debug)]
struct DockResize<W: LayoutElement> {
    window: W::Id,
    data: InteractiveResizeData,
    horizontal: Option<DividerDrag>,
    vertical: Option<DividerDrag>,
}

/// Stand-in for the scrolling view offset, for code that checks for view gestures.
#[cfg(test)]
pub(super) struct NoViewOffset;

#[cfg(test)]
impl NoViewOffset {
    pub fn is_gesture(&self) -> bool {
        false
    }

    pub fn is_animation_ongoing(&self) -> bool {
        false
    }
}

/// A docking-tree tiling space for windows.
#[derive(Debug)]
pub struct DockSpace<W: LayoutElement> {
    root: Option<Node<W>>,

    /// The active leaf. `Some` if and only if the tree is non-empty.
    active: Option<LeafId>,

    /// Ongoing interactive resize.
    interactive_resize: Option<DockResize<W>>,

    /// Windows in the closing animation.
    closing_windows: Vec<ClosingWindow>,

    view_size: Size<f64, Logical>,

    /// Working area including struts.
    working_area: Rect,

    /// Working area excluding struts (used for maximize and popups).
    parent_area: Rect,

    scale: f64,
    clock: Clock,
    options: Rc<Options>,
}

impl<W: LayoutElement> DockLeaf<W> {
    fn new(
        mut tile: Tile<W>,
        view_size: Size<f64, Logical>,
        scale: f64,
        options: Rc<Options>,
    ) -> Self {
        tile.update_config(view_size, scale, options);
        let mode = tile.window().pending_sizing_mode();
        Self {
            id: LeafId::next(),
            tile,
            is_pending_fullscreen: mode.is_fullscreen(),
            is_pending_maximized: mode.is_maximized(),
            rect: Rect::default(),
        }
    }

    pub fn contains(&self, window: &W::Id) -> bool {
        self.tile.window().id() == window
    }

    pub fn is_pending_fullscreen(&self) -> bool {
        self.is_pending_fullscreen
    }

    pub fn is_pending_maximized(&self) -> bool {
        self.is_pending_maximized
    }

    pub fn tiles(&self) -> impl Iterator<Item = (&Tile<W>, Point<f64, Logical>)> + '_ {
        std::iter::once((&self.tile, self.rect.loc))
    }

    fn pending_sizing_mode(&self) -> SizingMode {
        if self.is_pending_fullscreen {
            SizingMode::Fullscreen
        } else if self.is_pending_maximized {
            SizingMode::Maximized
        } else {
            SizingMode::Normal
        }
    }

    /// Position of the tile, not including the render offset.
    ///
    /// Follows the tile's *current* sizing mode so that a tile stays in place until it actually
    /// becomes fullscreen or maximized.
    fn origin(&self, parent_area: Rect) -> Point<f64, Logical> {
        match self.tile.sizing_mode() {
            SizingMode::Normal => self.rect.loc,
            SizingMode::Maximized => parent_area.loc,
            SizingMode::Fullscreen => Point::from((0., 0.)),
        }
    }

    fn request_size(&mut self, parent_area: Rect, animate: bool, transaction: Option<Transaction>) {
        match self.pending_sizing_mode() {
            SizingMode::Fullscreen => self.tile.request_fullscreen(animate, transaction),
            SizingMode::Maximized => {
                self.tile
                    .request_maximized(parent_area.size, animate, transaction)
            }
            SizingMode::Normal => {
                let mut size = self.rect.size;
                let max_size = self.tile.max_size_nonfullscreen();
                if max_size.w > 0. {
                    size.w = f64::min(size.w, max_size.w);
                }
                if max_size.h > 0. {
                    size.h = f64::min(size.h, max_size.h);
                }
                self.tile.request_tile_size(size, animate, transaction);
            }
        }
    }
}

impl<W: LayoutElement> Node<W> {
    fn leaves<'a>(&'a self, out: &mut Vec<&'a DockLeaf<W>>) {
        match self {
            Node::Leaf(leaf) => out.push(leaf),
            Node::Split(split) => {
                for child in &split.children {
                    child.leaves(out);
                }
            }
        }
    }

    fn leaves_mut<'a>(&'a mut self, out: &mut Vec<&'a mut DockLeaf<W>>) {
        match self {
            Node::Leaf(leaf) => out.push(leaf),
            Node::Split(split) => {
                for child in &mut split.children {
                    child.leaves_mut(out);
                }
            }
        }
    }

    fn find_path(&self, pred: &dyn Fn(&DockLeaf<W>) -> bool, path: &mut Vec<usize>) -> bool {
        match self {
            Node::Leaf(leaf) => pred(leaf),
            Node::Split(split) => {
                for (idx, child) in split.children.iter().enumerate() {
                    path.push(idx);
                    if child.find_path(pred, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
        }
    }

    fn at(&self, path: &[usize]) -> &Node<W> {
        match path.split_first() {
            None => self,
            Some((&idx, rest)) => match self {
                Node::Split(split) => split.children[idx].at(rest),
                Node::Leaf(_) => panic!("dock path goes through a leaf"),
            },
        }
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut Node<W> {
        match path.split_first() {
            None => self,
            Some((&idx, rest)) => match self {
                Node::Split(split) => split.children[idx].at_mut(rest),
                Node::Leaf(_) => panic!("dock path goes through a leaf"),
            },
        }
    }

    fn as_split(&self) -> Option<&Split<W>> {
        match self {
            Node::Split(split) => Some(split),
            Node::Leaf(_) => None,
        }
    }

    fn as_split_mut(&mut self) -> Option<&mut Split<W>> {
        match self {
            Node::Split(split) => Some(split),
            Node::Leaf(_) => None,
        }
    }

    fn as_leaf(&self) -> Option<&DockLeaf<W>> {
        match self {
            Node::Leaf(leaf) => Some(&**leaf),
            Node::Split(_) => None,
        }
    }

    fn as_leaf_mut(&mut self) -> Option<&mut DockLeaf<W>> {
        match self {
            Node::Leaf(leaf) => Some(&mut **leaf),
            Node::Split(_) => None,
        }
    }

    fn layout(&mut self, rect: Rect, gaps: f64, scale: f64) {
        match self {
            Node::Leaf(leaf) => leaf.rect = rect,
            Node::Split(split) => {
                split.rect = rect;
                let rects = split_rects(rect, split.axis, &split.fractions, gaps, scale);
                for (child, rect) in zip(&mut split.children, rects) {
                    child.layout(rect, gaps, scale);
                }
            }
        }
    }
}

/// Normalizes a subtree: drops empty splits, collapses single-child splits, flattens nested
/// splits with the same axis, and renormalizes fractions.
fn normalize<W: LayoutElement>(node: Node<W>) -> Option<Node<W>> {
    let split = match node {
        Node::Leaf(leaf) => return Some(Node::Leaf(leaf)),
        Node::Split(split) => split,
    };

    let mut children = Vec::with_capacity(split.children.len());
    let mut fractions = Vec::with_capacity(split.children.len());
    for (child, frac) in zip(split.children, split.fractions) {
        let Some(child) = normalize(child) else {
            continue;
        };

        match child {
            Node::Split(inner) if inner.axis == split.axis => {
                for (c, f) in zip(inner.children, inner.fractions) {
                    children.push(c);
                    fractions.push(frac * f);
                }
            }
            child => {
                children.push(child);
                fractions.push(frac);
            }
        }
    }

    match children.len() {
        0 => None,
        1 => children.pop(),
        n => {
            renormalize(&mut fractions, n);
            Some(Node::Split(Split {
                axis: split.axis,
                children,
                fractions,
                rect: split.rect,
            }))
        }
    }
}

fn renormalize(fractions: &mut [f64], n: usize) {
    let sum: f64 = fractions.iter().sum();
    if sum.is_finite() && sum > 0. && fractions.iter().all(|f| f.is_finite() && *f > 0.) {
        for f in fractions.iter_mut() {
            *f /= sum;
        }
    } else {
        fractions.fill(1. / n as f64);
    }
}

fn round_phys(x: f64, scale: f64) -> f64 {
    (x * scale).round() / scale
}

/// Rectangles of the children of a split, with gaps between them, rounded to physical pixels.
fn split_rects(rect: Rect, axis: Axis, fractions: &[f64], gaps: f64, scale: f64) -> Vec<Rect> {
    let n = fractions.len();
    let (origin, len) = match axis {
        Axis::Horizontal => (rect.loc.x, rect.size.w),
        Axis::Vertical => (rect.loc.y, rect.size.h),
    };
    let avail = f64::max(0., len - gaps * n.saturating_sub(1) as f64);

    let mut rv = Vec::with_capacity(n);
    let mut cum = 0.;
    for (i, frac) in fractions.iter().enumerate() {
        let i_gaps = gaps * i as f64;
        let start = round_phys(origin + cum * avail + i_gaps, scale);
        cum += frac;
        let end = if i + 1 == n {
            round_phys(origin + len, scale)
        } else {
            round_phys(origin + cum * avail + i_gaps, scale)
        };
        let size = f64::max(0., end - start);

        let child = match axis {
            Axis::Horizontal => Rect::new(
                Point::from((start, rect.loc.y)),
                Size::from((size, rect.size.h)),
            ),
            Axis::Vertical => Rect::new(
                Point::from((rect.loc.x, start)),
                Size::from((rect.size.w, size)),
            ),
        };
        rv.push(child);
    }
    rv
}

fn equal_rect(rect: Rect, axis: Axis, n: usize, idx: usize, gaps: f64, scale: f64) -> Rect {
    let fractions = vec![1. / n as f64; n];
    split_rects(rect, axis, &fractions, gaps, scale)[idx]
}

/// Fractions after inserting a child at `at`, which takes half of the share of the child that is
/// currently at `take_from` (indices before the insertion). The other children keep their share.
fn fractions_after_insert(fractions: &[f64], at: usize, take_from: usize) -> Vec<f64> {
    let mut rv = fractions.to_vec();
    let half = rv[take_from] / 2.;
    rv[take_from] = half;
    rv.insert(at, half);
    rv
}

/// Fractions after inserting a child at the edge (`at` is 0 or `len`) of a split, taking an equal
/// share proportionally from all children.
fn fractions_after_edge_insert(fractions: &[f64], at: usize) -> Vec<f64> {
    let n = fractions.len() + 1;
    let new = 1. / n as f64;
    let mut rv: Vec<f64> = fractions.iter().map(|f| f * (1. - new)).collect();
    rv.insert(at, new);
    rv
}

/// Removes child `idx`'s fraction, giving its share to the adjacent sibling(s) proportionally to
/// their current shares.
fn remove_fraction(fractions: &mut Vec<f64>, idx: usize) {
    let freed = fractions.remove(idx);
    let n = fractions.len();
    if n == 0 {
        return;
    }

    let before = idx.checked_sub(1);
    let after = (idx < n).then_some(idx);
    match (before, after) {
        (Some(b), Some(a)) => {
            let sum = fractions[b] + fractions[a];
            if sum > 0. {
                let fb = fractions[b] / sum;
                fractions[b] += freed * fb;
                fractions[a] += freed * (1. - fb);
            } else {
                fractions[b] += freed / 2.;
                fractions[a] += freed / 2.;
            }
        }
        (Some(i), None) | (None, Some(i)) => fractions[i] += freed,
        (None, None) => unreachable!(),
    }
    renormalize(fractions, n);
}

fn axis_len(size: Size<f64, Logical>, axis: Axis) -> f64 {
    match axis {
        Axis::Horizontal => size.w,
        Axis::Vertical => size.h,
    }
}

fn split_avail(split: &Split<impl LayoutElement>, gaps: f64) -> f64 {
    let n = split.children.len();
    f64::max(
        0.,
        axis_len(split.rect.size, split.axis) - gaps * n.saturating_sub(1) as f64,
    )
}

fn min_frac(avail: f64, n: usize) -> f64 {
    let equal = 1. / n as f64;
    if avail > 0. {
        f64::min(MIN_CHILD_LEN / avail, equal)
    } else {
        equal
    }
}

/// Sets child `idx` to `frac`, scaling the other children proportionally.
fn set_fraction(fractions: &mut [f64], idx: usize, frac: f64, min: f64) {
    let n = fractions.len();
    if n < 2 {
        return;
    }

    let max_frac = f64::max(min, 1. - min * (n - 1) as f64);
    let frac = if frac.is_finite() {
        frac.clamp(min, max_frac)
    } else {
        fractions[idx]
    };

    let others: f64 = fractions
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != idx)
        .map(|(_, f)| *f)
        .sum();
    let rest = 1. - frac;

    for (i, f) in fractions.iter_mut().enumerate() {
        if i == idx {
            *f = frac;
        } else if others > 0. {
            *f = *f / others * rest;
        } else {
            *f = rest / (n - 1) as f64;
        }
    }

    // Raise any child that fell below the minimum, taking the space from the resized child.
    let mut deficit = 0.;
    for (i, f) in fractions.iter_mut().enumerate() {
        if i != idx && *f < min {
            deficit += min - *f;
            *f = min;
        }
    }
    fractions[idx] = f64::max(min, fractions[idx] - deficit);

    renormalize(fractions, n);
}

fn rect_center(rect: Rect) -> Point<f64, Logical> {
    rect.loc + rect.size.downscale(2.).to_point()
}

fn distance_to_rect(rect: Rect, pos: Point<f64, Logical>) -> f64 {
    let dx = f64::max(
        0.,
        f64::max(rect.loc.x - pos.x, pos.x - (rect.loc.x + rect.size.w)),
    );
    let dy = f64::max(
        0.,
        f64::max(rect.loc.y - pos.y, pos.y - (rect.loc.y + rect.size.h)),
    );
    dx.hypot(dy)
}

/// Side of `rect` nearest to `pos`, normalized by the rect size (diagonal "X" regions).
fn nearest_side(rect: Rect, pos: Point<f64, Logical>) -> DockSide {
    let w = f64::max(rect.size.w, 1.);
    let h = f64::max(rect.size.h, 1.);
    let rel_x = (pos.x - rect.loc.x) / w;
    let rel_y = (pos.y - rect.loc.y) / h;

    let candidates = [
        (DockSide::Left, rel_x),
        (DockSide::Right, 1. - rel_x),
        (DockSide::Top, rel_y),
        (DockSide::Bottom, 1. - rel_y),
    ];
    candidates
        .into_iter()
        .min_by(|(_, a), (_, b)| f64::total_cmp(a, b))
        .map(|(side, _)| side)
        .unwrap()
}

/// Score of `to` as a directional neighbor of `from`, lower is better.
///
/// Returns `None` if `to` is not in that direction at all.
fn direction_score(from: Rect, to: Rect, dir: DockSide, slack: f64) -> Option<(bool, f64, f64)> {
    let (f_lo, f_hi, t_lo, t_hi) = match dir.axis() {
        Axis::Horizontal => (
            from.loc.x,
            from.loc.x + from.size.w,
            to.loc.x,
            to.loc.x + to.size.w,
        ),
        Axis::Vertical => (
            from.loc.y,
            from.loc.y + from.size.h,
            to.loc.y,
            to.loc.y + to.size.h,
        ),
    };
    let (p_f_lo, p_f_hi, p_t_lo, p_t_hi) = match dir.axis() {
        Axis::Horizontal => (
            from.loc.y,
            from.loc.y + from.size.h,
            to.loc.y,
            to.loc.y + to.size.h,
        ),
        Axis::Vertical => (
            from.loc.x,
            from.loc.x + from.size.w,
            to.loc.x,
            to.loc.x + to.size.w,
        ),
    };

    let edge_dist = if dir.is_before() {
        if t_hi > f_lo + slack {
            return None;
        }
        f_lo - t_hi
    } else {
        if t_lo < f_hi - slack {
            return None;
        }
        t_lo - f_hi
    };

    let overlap = f64::min(p_f_hi, p_t_hi) - f64::max(p_f_lo, p_t_lo);
    let perp_center_dist = ((p_f_lo + p_f_hi) / 2. - (p_t_lo + p_t_hi) / 2.).abs();

    Some((overlap <= 0., edge_dist.max(0.).round(), perp_center_dist))
}

impl<W: LayoutElement> DockSpace<W> {
    pub fn new(
        view_size: Size<f64, Logical>,
        parent_area: Rect,
        scale: f64,
        clock: Clock,
        options: Rc<Options>,
    ) -> Self {
        let working_area = compute_working_area(parent_area, scale, options.layout.struts);

        Self {
            root: None,
            active: None,
            interactive_resize: None,
            closing_windows: Vec::new(),
            view_size,
            working_area,
            parent_area,
            scale,
            clock,
            options,
        }
    }

    pub fn update_config(
        &mut self,
        view_size: Size<f64, Logical>,
        parent_area: Rect,
        scale: f64,
        options: Rc<Options>,
    ) {
        let working_area = compute_working_area(parent_area, scale, options.layout.struts);

        for leaf in self.leaves_mut() {
            leaf.tile.update_config(view_size, scale, options.clone());
        }

        self.view_size = view_size;
        self.working_area = working_area;
        self.parent_area = parent_area;
        self.scale = scale;
        self.options = options;

        self.relayout(false, false, None);
    }

    pub fn update_shaders(&mut self) {
        for leaf in self.leaves_mut() {
            leaf.tile.update_shaders();
        }
    }

    pub fn advance_animations(&mut self) {
        for leaf in self.leaves_mut() {
            leaf.tile.advance_animations();
        }

        self.closing_windows.retain_mut(|closing| {
            closing.advance_animations();
            closing.are_animations_ongoing()
        });
    }

    pub fn are_animations_ongoing(&self) -> bool {
        self.leaves()
            .iter()
            .any(|leaf| leaf.tile.are_animations_ongoing())
            || !self.closing_windows.is_empty()
    }

    pub fn are_transitions_ongoing(&self) -> bool {
        self.leaves()
            .iter()
            .any(|leaf| leaf.tile.are_transitions_ongoing())
            || !self.closing_windows.is_empty()
    }

    pub fn update_render_elements(&mut self, is_active: bool) {
        let active = self.active;
        let parent_area = self.parent_area;
        let view_size = self.view_size;
        for leaf in self.leaves_mut() {
            let is_active = is_active && Some(leaf.id) == active;
            let pos = leaf.origin(parent_area) + leaf.tile.render_offset();
            let view_rect = Rect::new(pos.upscale(-1.), view_size);
            leaf.tile.update_render_elements(is_active, view_rect);
        }
    }

    // ---------------------------------------------------------------------------------------
    // Tree access helpers.

    fn leaves(&self) -> Vec<&DockLeaf<W>> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.leaves(&mut out);
        }
        out
    }

    fn leaves_mut(&mut self) -> Vec<&mut DockLeaf<W>> {
        let mut out = Vec::new();
        if let Some(root) = &mut self.root {
            root.leaves_mut(&mut out);
        }
        out
    }

    fn find_path(&self, pred: &dyn Fn(&DockLeaf<W>) -> bool) -> Option<Vec<usize>> {
        let root = self.root.as_ref()?;
        let mut path = Vec::new();
        root.find_path(pred, &mut path).then_some(path)
    }

    fn path_of_window(&self, window: &W::Id) -> Option<Vec<usize>> {
        self.find_path(&|leaf| leaf.contains(window))
    }

    fn path_of_leaf(&self, id: LeafId) -> Option<Vec<usize>> {
        self.find_path(&|leaf| leaf.id == id)
    }

    fn node(&self, path: &[usize]) -> &Node<W> {
        self.root.as_ref().unwrap().at(path)
    }

    fn node_mut(&mut self, path: &[usize]) -> &mut Node<W> {
        self.root.as_mut().unwrap().at_mut(path)
    }

    fn leaf_at(&self, path: &[usize]) -> &DockLeaf<W> {
        self.node(path).as_leaf().unwrap()
    }

    fn leaf_by_id(&self, id: LeafId) -> Option<&DockLeaf<W>> {
        self.leaves().into_iter().find(|leaf| leaf.id == id)
    }

    fn leaf_by_window(&self, window: &W::Id) -> Option<&DockLeaf<W>> {
        self.leaves().into_iter().find(|leaf| leaf.contains(window))
    }

    fn leaf_by_window_mut(&mut self, window: &W::Id) -> Option<&mut DockLeaf<W>> {
        self.leaves_mut()
            .into_iter()
            .find(|leaf| leaf.contains(window))
    }

    fn active_leaf(&self) -> Option<&DockLeaf<W>> {
        self.leaf_by_id(self.active?)
    }

    fn active_leaf_mut(&mut self) -> Option<&mut DockLeaf<W>> {
        let active = self.active?;
        self.leaves_mut().into_iter().find(|leaf| leaf.id == active)
    }

    /// Path to the target window's leaf, or the active leaf if `window` is `None`.
    fn target_path(&self, window: Option<&W::Id>) -> Option<Vec<usize>> {
        match window {
            Some(window) => self.path_of_window(window),
            None => self.path_of_leaf(self.active?),
        }
    }

    fn root_rect(&self) -> Rect {
        let gaps = self.options.layout.gaps;
        let wa = self.working_area;
        let loc = Point::from((
            round_phys(wa.loc.x + gaps, self.scale),
            round_phys(wa.loc.y + gaps, self.scale),
        ));
        let right = round_phys(wa.loc.x + wa.size.w - gaps, self.scale);
        let bottom = round_phys(wa.loc.y + wa.size.h - gaps, self.scale);
        let size = Size::from((f64::max(0., right - loc.x), f64::max(0., bottom - loc.y)));
        Rect::new(loc, size)
    }

    /// Recomputes leaf rectangles and requests the corresponding tile sizes.
    fn relayout(
        &mut self,
        animate_moves: bool,
        animate_resize: bool,
        transaction: Option<Transaction>,
    ) {
        let parent_area = self.parent_area;
        let old: HashMap<LeafId, Point<f64, Logical>> = self
            .leaves()
            .into_iter()
            .map(|leaf| (leaf.id, leaf.origin(parent_area)))
            .collect();

        let rect = self.root_rect();
        let gaps = self.options.layout.gaps;
        let scale = self.scale;
        if let Some(root) = &mut self.root {
            root.layout(rect, gaps, scale);
        }

        for leaf in self.leaves_mut() {
            leaf.request_size(parent_area, animate_resize, transaction.clone());

            if animate_moves {
                if let Some(prev) = old.get(&leaf.id) {
                    let delta = *prev - leaf.origin(parent_area);
                    if delta != Point::from((0., 0.)) {
                        leaf.tile.animate_move_from(delta);
                    }
                }
            }
        }
    }

    fn cancel_interactive_resize(&mut self) {
        if let Some(resize) = self.interactive_resize.take() {
            if let Some(leaf) = self.leaf_by_window_mut(&resize.window) {
                leaf.tile.window_mut().cancel_interactive_resize();
            }
        }
    }

    fn default_insert(&self) -> DockInsert {
        match self.active {
            Some(id) => DockInsert {
                target: DockTarget::Leaf(id),
                side: DockSide::Right,
            },
            None => DockInsert {
                target: DockTarget::Root,
                side: DockSide::Right,
            },
        }
    }

    /// Inserts a leaf into the tree. Does not relayout.
    fn insert_leaf(&mut self, leaf: DockLeaf<W>, dock: DockInsert) {
        self.cancel_interactive_resize();

        let Some(root) = self.root.take() else {
            self.root = Some(Node::Leaf(Box::new(leaf)));
            return;
        };
        self.root = Some(root);

        let axis = dock.side.axis();
        let before = dock.side.is_before();

        let target_path = match dock.target {
            DockTarget::Root => None,
            DockTarget::Leaf(id) => match self.path_of_leaf(id) {
                Some(path) => Some(path),
                // Stale target: fall back to the default position.
                None => self.active.and_then(|id| self.path_of_leaf(id)),
            },
        };

        let new = Node::Leaf(Box::new(leaf));

        match target_path {
            None => {
                let root = self.root.as_mut().unwrap();
                match root {
                    Node::Split(split) if split.axis == axis => {
                        let idx = if before { 0 } else { split.children.len() };
                        split.children.insert(idx, new);
                        split.fractions = fractions_after_edge_insert(&split.fractions, idx);
                    }
                    _ => wrap_in_split(root, new, axis, before),
                }
            }
            Some(path) => {
                if let Some((&idx, parent_path)) = path.split_last() {
                    let parent = self.node_mut(parent_path).as_split_mut().unwrap();
                    if parent.axis == axis {
                        // Split the target's share; the other siblings keep theirs.
                        let at = if before { idx } else { idx + 1 };
                        parent.children.insert(at, new);
                        parent.fractions = fractions_after_insert(&parent.fractions, at, idx);
                        return;
                    }
                }

                let node = self.node_mut(&path);
                wrap_in_split(node, new, axis, before);
            }
        }

        self.root = self.root.take().and_then(normalize);
    }

    /// Removes the leaf at `path` from the tree. Does not relayout.
    fn take_leaf(&mut self, path: &[usize]) -> DockLeaf<W> {
        let resizing_this = self
            .interactive_resize
            .as_ref()
            .is_some_and(|resize| self.leaf_at(path).contains(&resize.window));
        if resizing_this {
            self.interactive_resize = None;
        } else {
            self.cancel_interactive_resize();
        }

        let leaf = match path.split_last() {
            None => match self.root.take() {
                Some(Node::Leaf(leaf)) => *leaf,
                _ => unreachable!("empty dock path must point at a root leaf"),
            },
            Some((&idx, parent_path)) => {
                let parent = self.node_mut(parent_path).as_split_mut().unwrap();
                let node = parent.children.remove(idx);
                remove_fraction(&mut parent.fractions, idx);
                match node {
                    Node::Leaf(leaf) => *leaf,
                    Node::Split(_) => unreachable!("dock path must point at a leaf"),
                }
            }
        };

        self.root = self.root.take().and_then(normalize);

        if self.active == Some(leaf.id) {
            // Activate the remaining leaf nearest to the removed one; usually the one that takes
            // over its space.
            let center = rect_center(leaf.rect);
            self.active = self
                .leaves()
                .into_iter()
                .min_by(|a, b| {
                    let da = rect_center(a.rect) - center;
                    let db = rect_center(b.rect) - center;
                    f64::total_cmp(&da.x.hypot(da.y), &db.x.hypot(db.y))
                })
                .map(|leaf| leaf.id);
        }

        leaf
    }

    fn swap_leaves(&mut self, a: &[usize], b: &[usize]) {
        let common = zip(a, b).take_while(|(x, y)| x == y).count();
        assert!(
            common < a.len() && common < b.len(),
            "can't swap a leaf with itself"
        );

        let split = self.node_mut(&a[..common]).as_split_mut().unwrap();
        let (i, j) = (a[common], b[common]);
        let (lo, hi, rest_lo, rest_hi) = if i < j {
            (i, j, &a[common + 1..], &b[common + 1..])
        } else {
            (j, i, &b[common + 1..], &a[common + 1..])
        };
        let (left, right) = split.children.split_at_mut(hi);
        let leaf_lo = left[lo].at_mut(rest_lo).as_leaf_mut().unwrap();
        let leaf_hi = right[0].at_mut(rest_hi).as_leaf_mut().unwrap();
        std::mem::swap(leaf_lo, leaf_hi);
    }

    fn neighbor_in_direction(&self, from: LeafId, dir: DockSide) -> Option<LeafId> {
        let from_rect = self.leaf_by_id(from)?.rect;
        let slack = self.options.layout.gaps + 1.;
        self.leaves()
            .into_iter()
            .filter(|leaf| leaf.id != from)
            .filter_map(|leaf| {
                direction_score(from_rect, leaf.rect, dir, slack).map(|score| (leaf.id, score))
            })
            .min_by(|(_, a), (_, b)| {
                a.0.cmp(&b.0)
                    .then(f64::total_cmp(&a.1, &b.1))
                    .then(f64::total_cmp(&a.2, &b.2))
            })
            .map(|(id, _)| id)
    }

    fn focus_direction(&mut self, dir: DockSide) -> bool {
        let Some(active) = self.active else {
            return false;
        };
        match self.neighbor_in_direction(active, dir) {
            Some(id) => {
                self.activate_leaf(id);
                true
            }
            None => false,
        }
    }

    fn move_direction(&mut self, dir: DockSide) -> bool {
        let Some(active) = self.active else {
            return false;
        };
        let Some(neighbor) = self.neighbor_in_direction(active, dir) else {
            return false;
        };

        let a = self.path_of_leaf(active).unwrap();
        let b = self.path_of_leaf(neighbor).unwrap();
        self.cancel_interactive_resize();
        self.swap_leaves(&a, &b);
        self.relayout(true, true, None);
        true
    }

    fn activate_leaf(&mut self, id: LeafId) {
        self.active = Some(id);
        if let Some(leaf) = self.active_leaf_mut() {
            leaf.tile.ensure_alpha_animates_to_1();
        }
    }

    /// Finds the nearest ancestor split of `axis` and the index of the branch containing the
    /// leaf at `path`.
    fn ancestor_split(&self, path: &[usize], axis: Axis) -> Option<(usize, usize)> {
        (0..path.len()).rev().find_map(|k| {
            let split = self.node(&path[..k]).as_split()?;
            (split.axis == axis).then_some((k, path[k]))
        })
    }

    /// Resizes the branch containing the leaf at `path` along `axis` to the given tile length.
    fn resize_branch(
        &mut self,
        path: &[usize],
        axis: Axis,
        target_len: impl FnOnce(f64, f64, &Tile<W>) -> Option<f64>,
        animate: bool,
    ) {
        let Some((k, idx)) = self.ancestor_split(path, axis) else {
            return;
        };

        let gaps = self.options.layout.gaps;
        let split = self.node(&path[..k]).as_split().unwrap();
        let avail = split_avail(split, gaps);
        if avail <= 0. {
            return;
        }
        let current = split.fractions[idx] * avail;
        let tile = &self.leaf_at(path).tile;
        let Some(target) = target_len(current, avail, tile) else {
            return;
        };

        let split = self.node_mut(&path[..k]).as_split_mut().unwrap();
        let min = min_frac(avail, split.children.len());
        set_fraction(&mut split.fractions, idx, target / avail, min);

        self.relayout(animate, animate, None);
    }

    fn resize_with_change(&mut self, window: Option<&W::Id>, axis: Axis, change: SizeChange) {
        let Some(path) = self.target_path(window) else {
            return;
        };

        if let Some(resize) = &self.interactive_resize {
            if self.leaf_at(&path).contains(&resize.window) {
                self.cancel_interactive_resize();
            }
        }

        self.resize_branch(
            &path,
            axis,
            |current, avail, tile| {
                let to_tile = |px: f64| match axis {
                    Axis::Horizontal => tile.tile_width_for_window_width(px),
                    Axis::Vertical => tile.tile_height_for_window_height(px),
                };
                Some(match change {
                    SizeChange::SetFixed(px) => to_tile(f64::from(px)),
                    SizeChange::SetProportion(prop) => avail * prop / 100.,
                    SizeChange::AdjustFixed(delta) => current + f64::from(delta),
                    SizeChange::AdjustProportion(prop) => current + avail * prop / 100.,
                })
            },
            true,
        );
    }

    fn toggle_preset(&mut self, window: Option<&W::Id>, axis: Axis, forwards: bool) {
        let Some(path) = self.target_path(window) else {
            return;
        };
        let presets = match axis {
            Axis::Horizontal => self.options.layout.preset_column_widths.clone(),
            Axis::Vertical => self.options.layout.preset_window_heights.clone(),
        };
        if presets.is_empty() {
            return;
        }

        self.resize_branch(
            &path,
            axis,
            |current, avail, tile| {
                let lens: Vec<f64> = presets
                    .iter()
                    .map(|preset| match *preset {
                        PresetSize::Proportion(prop) => avail * prop,
                        PresetSize::Fixed(px) => match axis {
                            Axis::Horizontal => tile.tile_width_for_window_width(f64::from(px)),
                            Axis::Vertical => tile.tile_height_for_window_height(f64::from(px)),
                        },
                    })
                    .collect();

                let next = if forwards {
                    lens.iter()
                        .copied()
                        .find(|len| *len > current + 1.)
                        .unwrap_or(lens[0])
                } else {
                    lens.iter()
                        .copied()
                        .rev()
                        .find(|len| *len < current - 1.)
                        .unwrap_or(lens[lens.len() - 1])
                };
                Some(next)
            },
            true,
        );
    }

    fn divider_for(&self, path: &[usize], axis: Axis, before: bool) -> Option<DividerDrag> {
        (0..path.len()).rev().find_map(|k| {
            let split = self.node(&path[..k]).as_split()?;
            if split.axis != axis {
                return None;
            }
            let idx = path[k];
            let neighbor = if before {
                idx.checked_sub(1)?
            } else {
                (idx + 1 < split.children.len()).then_some(idx + 1)?
            };
            Some(DividerDrag {
                split_path: path[..k].to_vec(),
                idx,
                neighbor,
                orig_frac: split.fractions[idx],
                orig_neighbor_frac: split.fractions[neighbor],
            })
        })
    }

    fn drag_divider(&mut self, drag: &DividerDrag, delta: f64) {
        let gaps = self.options.layout.gaps;
        let Some(root) = &mut self.root else {
            return;
        };

        // Validate that the path still points at a compatible split.
        let mut node = &mut *root;
        for &idx in &drag.split_path {
            match node {
                Node::Split(split) if idx < split.children.len() => {
                    node = &mut split.children[idx];
                }
                _ => return,
            }
        }
        let Node::Split(split) = node else {
            return;
        };
        let n = split.children.len();
        if drag.idx >= n || drag.neighbor >= n {
            return;
        }

        let avail = split_avail(split, gaps);
        if avail <= 0. {
            return;
        }

        let total = drag.orig_frac + drag.orig_neighbor_frac;
        let min = min_frac(avail, n);
        if total < min * 2. {
            return;
        }

        let frac = (drag.orig_frac + delta / avail).clamp(min, total - min);
        split.fractions[drag.idx] = frac;
        split.fractions[drag.neighbor] = total - frac;
        renormalize(&mut split.fractions, n);
    }

    fn hint_rect(&self, dock: DockInsert) -> Rect {
        let root_rect = self.root_rect();
        let Some(root) = &self.root else {
            return root_rect;
        };

        let gaps = self.options.layout.gaps;
        let scale = self.scale;
        let axis = dock.side.axis();
        let before = dock.side.is_before();

        match dock.target {
            DockTarget::Root => match root {
                Node::Split(split) if split.axis == axis => {
                    let idx = if before { 0 } else { split.children.len() };
                    let fractions = fractions_after_edge_insert(&split.fractions, idx);
                    split_rects(root_rect, axis, &fractions, gaps, scale)[idx]
                }
                _ => equal_rect(root_rect, axis, 2, usize::from(!before), gaps, scale),
            },
            DockTarget::Leaf(id) => {
                let Some(path) = self.path_of_leaf(id) else {
                    return root_rect;
                };
                if let Some((&idx, parent_path)) = path.split_last() {
                    let parent = self.node(parent_path).as_split().unwrap();
                    if parent.axis == axis {
                        let at = if before { idx } else { idx + 1 };
                        let fractions = fractions_after_insert(&parent.fractions, at, idx);
                        return split_rects(parent.rect, axis, &fractions, gaps, scale)[at];
                    }
                }
                let rect = self.leaf_at(&path).rect;
                equal_rect(rect, axis, 2, usize::from(!before), gaps, scale)
            }
        }
    }

    fn default_width(&self) -> ColumnWidth {
        self.options
            .layout
            .default_column_width
            .map(ColumnWidth::from)
            .unwrap_or(ColumnWidth::Proportion(0.5))
    }

    fn leaves_in_render_order(&self) -> Vec<&DockLeaf<W>> {
        let active = self.active;
        let mut leaves = self.leaves();
        leaves.sort_by_key(|leaf| render_rank(leaf, active));
        leaves
    }

    // ---------------------------------------------------------------------------------------
    // Queries.

    pub fn tiles(&self) -> impl Iterator<Item = &Tile<W>> + '_ {
        self.leaves().into_iter().map(|leaf| &leaf.tile)
    }

    pub fn tiles_mut(&mut self) -> impl Iterator<Item = &mut Tile<W>> + '_ {
        self.leaves_mut().into_iter().map(|leaf| &mut leaf.tile)
    }

    /// Leaves of the tree in tree order, standing in for scrolling columns.
    pub fn columns(&self) -> impl Iterator<Item = &DockLeaf<W>> {
        self.leaves().into_iter()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn active_window(&self) -> Option<&W> {
        self.active_leaf().map(|leaf| leaf.tile.window())
    }

    pub fn active_window_mut(&mut self) -> Option<&mut W> {
        self.active_leaf_mut().map(|leaf| leaf.tile.window_mut())
    }

    pub fn active_tile_mut(&mut self) -> Option<&mut Tile<W>> {
        self.active_leaf_mut().map(|leaf| &mut leaf.tile)
    }

    pub fn is_active_pending_fullscreen(&self) -> bool {
        self.active_leaf()
            .is_some_and(|leaf| leaf.pending_sizing_mode().is_fullscreen())
    }

    pub fn new_window_toplevel_bounds(&self, rules: &ResolvedWindowRules) -> Size<i32, Logical> {
        let border_config = self.options.layout.border.merged_with(&rules.border);
        compute_toplevel_bounds(
            border_config,
            self.working_area.size,
            Size::from((0., 0.)),
            self.options.layout.gaps,
        )
    }

    pub fn new_window_size(
        &self,
        _width: Option<PresetSize>,
        _height: Option<PresetSize>,
        rules: &ResolvedWindowRules,
    ) -> Size<i32, Logical> {
        // The window will take the rectangle of its dock slot.
        let rect = self.hint_rect(self.default_insert());
        let border = self.options.layout.border.merged_with(&rules.border);
        let mut size = rect.size;
        if !border.off {
            size.w -= border.width * 2.;
            size.h -= border.width * 2.;
        }
        Size::from((max(1, size.w.floor() as i32), max(1, size.h.floor() as i32)))
    }

    pub(super) fn insert_position(&self, pos: Point<f64, Logical>) -> InsertPosition {
        let Some(_) = &self.root else {
            return InsertPosition::Dock(DockInsert {
                target: DockTarget::Root,
                side: DockSide::Right,
            });
        };

        // Near the working area edge: dock beside everything.
        let wa = self.working_area;
        let edges = [
            (DockSide::Left, pos.x - wa.loc.x),
            (DockSide::Right, wa.loc.x + wa.size.w - pos.x),
            (DockSide::Top, pos.y - wa.loc.y),
            (DockSide::Bottom, wa.loc.y + wa.size.h - pos.y),
        ];
        let (side, dist) = edges
            .into_iter()
            .min_by(|(_, a), (_, b)| f64::total_cmp(a, b))
            .unwrap();
        if dist < ROOT_EDGE_BAND {
            return InsertPosition::Dock(DockInsert {
                target: DockTarget::Root,
                side,
            });
        }

        // Otherwise dock beside the leaf under (or nearest to) the pointer, on the nearest side.
        //
        // FIXME: the center of a leaf will become a tab group target.
        let leaf = self
            .leaves()
            .into_iter()
            .min_by(|a, b| {
                f64::total_cmp(
                    &distance_to_rect(a.rect, pos),
                    &distance_to_rect(b.rect, pos),
                )
            })
            .unwrap();

        InsertPosition::Dock(DockInsert {
            target: DockTarget::Leaf(leaf.id),
            side: nearest_side(leaf.rect, pos),
        })
    }

    /// Finds the divider between two docked siblings under `pos`.
    ///
    /// Returns a window adjacent to the divider and the edge of that window to resize, such that
    /// `interactive_resize_begin(window, edges)` drags exactly that divider.
    pub fn divider_under(&self, pos: Point<f64, Logical>) -> Option<(W::Id, ResizeEdge)> {
        let root = self.root.as_ref()?;
        // Fullscreen and maximized windows cover the dividers.
        if self
            .leaves()
            .iter()
            .any(|leaf| !leaf.pending_sizing_mode().is_normal())
        {
            return None;
        }

        let gaps = self.options.layout.gaps;
        let pad = f64::max(0., (DIVIDER_HIT - gaps) / 2.);
        divider_under(root, pos, pad).map(|(leaf, edge)| (leaf.tile.window().id().clone(), edge))
    }

    /// Where a window dragged from the floating Desktop lands: docked if the pointer is over the
    /// working area edge band or the edge band of a docked window, otherwise back on the Desktop.
    pub(super) fn floating_drop_position(&self, pos: Point<f64, Logical>) -> InsertPosition {
        // Near the working area edge: dock beside everything.
        let wa = self.working_area;
        if wa.contains(pos) {
            let edges = [
                (DockSide::Left, pos.x - wa.loc.x),
                (DockSide::Right, wa.loc.x + wa.size.w - pos.x),
                (DockSide::Top, pos.y - wa.loc.y),
                (DockSide::Bottom, wa.loc.y + wa.size.h - pos.y),
            ];
            let (side, dist) = edges
                .into_iter()
                .min_by(|(_, a), (_, b)| f64::total_cmp(a, b))
                .unwrap();
            if dist < ROOT_EDGE_BAND {
                return InsertPosition::Dock(DockInsert {
                    target: DockTarget::Root,
                    side,
                });
            }
        }

        // Over the edge band of a docked window: dock beside it.
        for leaf in self.leaves() {
            let rect = leaf.rect;
            if !rect.contains(pos) {
                continue;
            }
            let band_x = f64::min(LEAF_EDGE_BAND, rect.size.w / 4.);
            let band_y = f64::min(LEAF_EDGE_BAND, rect.size.h / 4.);
            let edges = [
                (DockSide::Left, (pos.x - rect.loc.x) / band_x),
                (DockSide::Right, (rect.loc.x + rect.size.w - pos.x) / band_x),
                (DockSide::Top, (pos.y - rect.loc.y) / band_y),
                (
                    DockSide::Bottom,
                    (rect.loc.y + rect.size.h - pos.y) / band_y,
                ),
            ];
            let (side, rel) = edges
                .into_iter()
                .min_by(|(_, a), (_, b)| f64::total_cmp(a, b))
                .unwrap();
            if rel < 1. {
                return InsertPosition::Dock(DockInsert {
                    target: DockTarget::Leaf(leaf.id),
                    side,
                });
            }
        }

        InsertPosition::Floating
    }

    pub(super) fn insert_hint_area(&self, position: InsertPosition) -> Option<Rect> {
        match position {
            InsertPosition::Dock(dock) => Some(self.hint_rect(dock)),
            InsertPosition::NewColumn(_) | InsertPosition::InColumn(_, _) => {
                Some(self.hint_rect(self.default_insert()))
            }
            InsertPosition::Floating => None,
        }
    }

    pub fn tiles_with_render_positions(
        &self,
    ) -> impl Iterator<Item = (&Tile<W>, Point<f64, Logical>, bool)> {
        let scale = self.scale;
        let parent_area = self.parent_area;
        self.leaves_in_render_order().into_iter().map(move |leaf| {
            let pos = leaf.origin(parent_area) + leaf.tile.render_offset();
            // Round to physical pixels.
            let pos = pos.to_physical_precise_round(scale).to_logical(scale);
            (&leaf.tile, pos, true)
        })
    }

    pub fn tiles_with_render_positions_mut(
        &mut self,
        round: bool,
    ) -> impl Iterator<Item = (&mut Tile<W>, Point<f64, Logical>)> {
        let scale = self.scale;
        let parent_area = self.parent_area;
        let active = self.active;
        let mut leaves = self.leaves_mut();
        leaves.sort_by_key(|leaf| render_rank(leaf, active));
        leaves.into_iter().map(move |leaf| {
            let mut pos = leaf.origin(parent_area) + leaf.tile.render_offset();
            if round {
                pos = pos.to_physical_precise_round(scale).to_logical(scale);
            }
            (&mut leaf.tile, pos)
        })
    }

    pub fn tiles_with_ipc_layouts(&self) -> impl Iterator<Item = (&Tile<W>, WindowLayout)> {
        let scale = self.scale;
        let parent_area = self.parent_area;
        self.leaves().into_iter().map(move |leaf| {
            let pos = leaf.origin(parent_area);
            let pos = pos.to_physical_precise_round(scale).to_logical(scale);
            let layout = WindowLayout {
                pos_in_scrolling_layout: None,
                tile_pos_in_workspace_view: Some(pos.into()),
                ..leaf.tile.ipc_layout_template()
            };
            (&leaf.tile, layout)
        })
    }

    /// Returns the geometry of the active tile relative to and clamped to the view.
    pub fn active_tile_visual_rectangle(&self) -> Option<Rect> {
        let leaf = self.active_leaf()?;
        let rect = Rect::new(leaf.origin(self.parent_area), leaf.tile.tile_size());
        Rect::from_size(self.view_size).intersection(rect)
    }

    pub fn popup_target_rect(&self, id: &W::Id) -> Option<Rect> {
        let leaf = self.leaf_by_window(id)?;
        let mut target = self.parent_area;
        target.loc -= leaf.origin(self.parent_area);
        target.loc -= leaf.tile.window_loc();
        Some(target)
    }

    pub fn render_above_top_layer(&self) -> bool {
        self.active_leaf()
            .is_some_and(|leaf| leaf.tile.sizing_mode().is_fullscreen())
    }

    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<(&W, HitType)> {
        // This matches self.tiles_with_render_positions().
        self.tiles_with_render_positions()
            .find_map(|(tile, tile_pos, _)| HitType::hit_tile(tile, tile_pos, pos))
    }

    pub fn scroll_amount_to_activate(&self, _window: &W::Id) -> f64 {
        0.
    }

    pub fn render<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        xray_pos: XrayPos,
        focus_ring: bool,
        push: &mut dyn FnMut(DockSpaceRenderElement<R>),
    ) {
        let scale = Scale::from(self.scale);

        // Draw the closing windows on top of the other windows.
        let view_rect = Rect::from_size(self.view_size);
        for closing in self.closing_windows.iter().rev() {
            let elem = closing.render(ctx.as_gles(), view_rect, scale);
            push(elem.into());
        }

        let active = self.active;
        let parent_area = self.parent_area;
        for leaf in self.leaves_in_render_order() {
            let tile_pos = leaf.origin(parent_area) + leaf.tile.render_offset();
            let tile_pos = tile_pos
                .to_physical_precise_round(self.scale)
                .to_logical(self.scale);

            let focus_ring = focus_ring && Some(leaf.id) == active;
            let xray_pos = xray_pos.offset(tile_pos);
            leaf.tile
                .render(ctx.r(), tile_pos, xray_pos, focus_ring, &mut |elem| {
                    push(elem.into())
                });
        }
    }

    // ---------------------------------------------------------------------------------------
    // Adding and removing.

    /// Docks a tile at the given position in the tree.
    pub fn add_tile_docked(&mut self, tile: Tile<W>, dock: DockInsert, activate: bool) {
        let leaf = DockLeaf::new(tile, self.view_size, self.scale, self.options.clone());
        let id = leaf.id;
        self.insert_leaf(leaf, dock);
        if activate || self.active.is_none() {
            self.activate_leaf(id);
        }
        self.relayout(true, true, None);
    }

    pub fn add_tile(
        &mut self,
        _col_idx: Option<usize>,
        tile: Tile<W>,
        activate: bool,
        _width: ColumnWidth,
        _is_full_width: bool,
        _anim_config: Option<niri_config::Animation>,
    ) {
        let dock = self.default_insert();
        self.add_tile_docked(tile, dock, activate);
    }

    pub fn add_tile_to_column(
        &mut self,
        col_idx: usize,
        _tile_idx: Option<usize>,
        tile: Tile<W>,
        activate: bool,
    ) {
        // Treat the column index as a leaf index in tree order and dock below that leaf.
        let dock = match self.leaves().get(col_idx) {
            Some(leaf) => DockInsert {
                target: DockTarget::Leaf(leaf.id),
                side: DockSide::Bottom,
            },
            None => self.default_insert(),
        };
        self.add_tile_docked(tile, dock, activate);
    }

    pub fn add_tile_right_of(
        &mut self,
        right_of: &W::Id,
        tile: Tile<W>,
        activate: bool,
        _width: ColumnWidth,
        _is_full_width: bool,
    ) {
        let dock = match self.leaf_by_window(right_of) {
            Some(leaf) => DockInsert {
                target: DockTarget::Leaf(leaf.id),
                side: DockSide::Right,
            },
            None => self.default_insert(),
        };
        self.add_tile_docked(tile, dock, activate);
    }

    pub fn add_column(
        &mut self,
        _idx: Option<usize>,
        column: Column<W>,
        activate: bool,
        _anim_config: Option<niri_config::Animation>,
    ) {
        // Dock the first tile next to the active leaf and stack the rest below it.
        let mut prev: Option<LeafId> = None;
        let (tiles, is_pending_maximized) = column.into_tiles();
        for tile in tiles {
            let dock = match prev {
                Some(id) => DockInsert {
                    target: DockTarget::Leaf(id),
                    side: DockSide::Bottom,
                },
                None => self.default_insert(),
            };
            let mut leaf = DockLeaf::new(tile, self.view_size, self.scale, self.options.clone());
            leaf.is_pending_maximized |= is_pending_maximized;
            let id = leaf.id;
            self.insert_leaf(leaf, dock);
            if (activate && prev.is_none()) || self.active.is_none() {
                self.activate_leaf(id);
            }
            prev = Some(id);
        }
        self.relayout(true, true, None);
    }

    pub fn remove_active_tile(&mut self, transaction: Transaction) -> Option<RemovedTile<W>> {
        let id = self.active_leaf()?.tile.window().id().clone();
        Some(self.remove_tile(&id, transaction))
    }

    pub fn remove_tile(&mut self, window: &W::Id, transaction: Transaction) -> RemovedTile<W> {
        let path = self
            .path_of_window(window)
            .expect("window must be in the dock tree");
        let leaf = self.take_leaf(&path);
        self.relayout(true, true, Some(transaction));

        RemovedTile {
            tile: leaf.tile,
            width: self.default_width(),
            is_full_width: false,
            is_floating: false,
        }
    }

    pub fn remove_active_column(&mut self) -> Option<Column<W>> {
        let was_maximized = self.active_leaf()?.is_pending_maximized;
        let removed = self.remove_active_tile(Transaction::new())?;
        let mut column = Column::new_with_tile(
            removed.tile,
            self.view_size,
            self.working_area,
            self.parent_area,
            self.scale,
            removed.width,
            removed.is_full_width,
        );
        if was_maximized {
            column.set_maximized(true);
        }
        Some(column)
    }

    pub fn update_window(&mut self, window: &W::Id, serial: Option<Serial>) {
        let parent_area = self.parent_area;
        let Some(leaf) = self.leaf_by_window_mut(window) else {
            return;
        };

        let prev_origin = leaf.origin(parent_area);

        // Do this before calling update_window() so it can get up-to-date info.
        if let Some(serial) = serial {
            leaf.tile.window_mut().on_commit(serial);
        }

        leaf.tile.update_window();

        // Going in and out of fullscreen/maximized moves the tile; animate that.
        let delta = prev_origin - leaf.origin(parent_area);
        if delta != Point::from((0., 0.)) {
            leaf.tile.animate_move_from(delta);
        }
    }

    pub fn activate_window(&mut self, window: &W::Id) -> bool {
        let Some(id) = self.leaf_by_window(window).map(|leaf| leaf.id) else {
            return false;
        };
        self.activate_leaf(id);
        true
    }

    pub fn start_close_animation_for_window(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &W::Id,
        blocker: TransactionBlocker,
    ) {
        let Some((tile, tile_pos)) = self
            .tiles_with_render_positions_mut(false)
            .find(|(tile, _)| tile.window().id() == window)
        else {
            return;
        };

        let Some(snapshot) = tile.take_unmap_snapshot() else {
            return;
        };

        let tile_size = tile.tile_size();
        self.start_close_animation_for_tile(renderer, snapshot, tile_size, tile_pos, blocker);
    }

    fn start_close_animation_for_tile(
        &mut self,
        renderer: &mut GlesRenderer,
        snapshot: TileRenderSnapshot,
        tile_size: Size<f64, Logical>,
        tile_pos: Point<f64, Logical>,
        blocker: TransactionBlocker,
    ) {
        let anim = Animation::new(
            self.clock.clone(),
            0.,
            1.,
            0.,
            self.options.animations.window_close.anim,
        );

        let blocker = if self.options.disable_transactions {
            TransactionBlocker::completed()
        } else {
            blocker
        };

        let scale = Scale::from(self.scale);
        let res = ClosingWindow::new(
            renderer, snapshot, scale, tile_size, tile_pos, blocker, anim,
        );
        match res {
            Ok(closing) => self.closing_windows.push(closing),
            Err(err) => warn!("error creating a closing window animation: {err:?}"),
        }
    }

    pub fn start_open_animation(&mut self, id: &W::Id) -> bool {
        match self.leaf_by_window_mut(id) {
            Some(leaf) => {
                leaf.tile.start_open_animation();
                true
            }
            None => false,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Focus and movement.

    pub fn focus_left(&mut self) -> bool {
        self.focus_direction(DockSide::Left)
    }

    pub fn focus_right(&mut self) -> bool {
        self.focus_direction(DockSide::Right)
    }

    pub fn focus_up(&mut self) -> bool {
        self.focus_direction(DockSide::Top)
    }

    pub fn focus_down(&mut self) -> bool {
        self.focus_direction(DockSide::Bottom)
    }

    pub fn focus_column_first(&mut self) {
        if let Some(id) = self.leaves().first().map(|leaf| leaf.id) {
            self.activate_leaf(id);
        }
    }

    pub fn focus_column_last(&mut self) {
        if let Some(id) = self.leaves().last().map(|leaf| leaf.id) {
            self.activate_leaf(id);
        }
    }

    /// Focuses the leaf with this 1-based index in tree order.
    pub fn focus_column(&mut self, index: usize) {
        let leaves = self.leaves();
        if leaves.is_empty() {
            return;
        }
        let idx = index.saturating_sub(1).min(leaves.len() - 1);
        let id = leaves[idx].id;
        self.activate_leaf(id);
    }

    pub fn focus_window_in_column(&mut self, _index: u8) {}

    pub fn focus_down_or_left(&mut self) {
        if !self.focus_down() {
            self.focus_left();
        }
    }

    pub fn focus_down_or_right(&mut self) {
        if !self.focus_down() {
            self.focus_right();
        }
    }

    pub fn focus_up_or_left(&mut self) {
        if !self.focus_up() {
            self.focus_left();
        }
    }

    pub fn focus_up_or_right(&mut self) {
        if !self.focus_up() {
            self.focus_right();
        }
    }

    pub fn focus_top(&mut self) {
        for _ in 0..self.leaves().len() {
            if !self.focus_up() {
                break;
            }
        }
    }

    pub fn focus_bottom(&mut self) {
        for _ in 0..self.leaves().len() {
            if !self.focus_down() {
                break;
            }
        }
    }

    pub fn move_left(&mut self) -> bool {
        self.move_direction(DockSide::Left)
    }

    pub fn move_right(&mut self) -> bool {
        self.move_direction(DockSide::Right)
    }

    pub fn move_up(&mut self) -> bool {
        self.move_direction(DockSide::Top)
    }

    pub fn move_down(&mut self) -> bool {
        self.move_direction(DockSide::Bottom)
    }

    pub fn move_column_to_index(&mut self, _index: usize) {}

    pub fn move_column_to_first(&mut self) {}

    pub fn move_column_to_last(&mut self) {}

    /// Stacks the window below its left neighbor, or, if it is already in a vertical stack,
    /// moves it out to the left of that stack.
    pub fn consume_or_expel_window_left(&mut self, window: Option<&W::Id>) {
        self.consume_or_expel(window, DockSide::Left);
    }

    /// Stacks the window below its right neighbor, or, if it is already in a vertical stack,
    /// moves it out to the right of that stack.
    pub fn consume_or_expel_window_right(&mut self, window: Option<&W::Id>) {
        self.consume_or_expel(window, DockSide::Right);
    }

    /// Stacks the right neighbor of the active window below it.
    pub fn consume_into_column(&mut self) {
        let Some(active) = self.active else {
            return;
        };
        let Some(neighbor) = self.neighbor_in_direction(active, DockSide::Right) else {
            return;
        };

        // Append to the bottom of the active window's stack, like consuming into a column.
        let mut target = active;
        let path = self.path_of_leaf(active).unwrap();
        if let Some((_, stack_path)) = path.split_last() {
            let stack = self.node(stack_path).as_split().unwrap();
            if stack.axis == Axis::Vertical {
                if let Some(last) = stack.children.last().and_then(Node::as_leaf) {
                    target = last.id;
                }
            }
        }

        self.relocate_leaf(
            neighbor,
            DockInsert {
                target: DockTarget::Leaf(target),
                side: DockSide::Bottom,
            },
        );
    }

    /// Moves the active window out of its vertical stack, to the right.
    pub fn expel_from_column(&mut self) {
        let Some(path) = self.target_path(None) else {
            return;
        };
        if self.parent_axis(&path) == Some(Axis::Vertical) {
            self.expel(&path, false);
        }
    }

    fn parent_axis(&self, path: &[usize]) -> Option<Axis> {
        let (_, parent_path) = path.split_last()?;
        self.node(parent_path).as_split().map(|split| split.axis)
    }

    fn consume_or_expel(&mut self, window: Option<&W::Id>, side: DockSide) {
        let Some(path) = self.target_path(window) else {
            return;
        };

        if self.parent_axis(&path) == Some(Axis::Vertical) {
            self.expel(&path, side.is_before());
            return;
        }

        let id = self.leaf_at(&path).id;
        if let Some(neighbor) = self.neighbor_in_direction(id, side) {
            self.relocate_leaf(
                id,
                DockInsert {
                    target: DockTarget::Leaf(neighbor),
                    side: DockSide::Bottom,
                },
            );
        }
    }

    /// Moves the leaf at `path` (whose parent is a vertical split) beside that split.
    fn expel(&mut self, path: &[usize], before: bool) {
        self.cancel_interactive_resize();

        let (&idx, stack_path) = path.split_last().unwrap();
        let stack = self.node_mut(stack_path).as_split_mut().unwrap();
        let node = stack.children.remove(idx);
        remove_fraction(&mut stack.fractions, idx);

        match stack_path.split_last() {
            None => {
                let root = self.root.as_mut().unwrap();
                wrap_in_split(root, node, Axis::Horizontal, before);
            }
            Some((&stack_idx, row_path)) => {
                // The tree is normalized, so the stack's parent is horizontal.
                let row = self.node_mut(row_path).as_split_mut().unwrap();
                let at = if before { stack_idx } else { stack_idx + 1 };
                row.children.insert(at, node);
                row.fractions = fractions_after_insert(&row.fractions, at, stack_idx);
            }
        }

        self.root = self.root.take().and_then(normalize);
        self.relayout(true, true, None);
    }

    /// Moves a leaf to another dock position, keeping focus as it was.
    fn relocate_leaf(&mut self, id: LeafId, dock: DockInsert) -> bool {
        if dock.target == DockTarget::Leaf(id) {
            return false;
        }
        let Some(path) = self.path_of_leaf(id) else {
            return false;
        };

        let prev_active = self.active;
        let leaf = self.take_leaf(&path);
        self.insert_leaf(leaf, dock);
        self.active = prev_active;
        self.relayout(true, true, None);
        true
    }

    pub fn swap_window_in_direction(&mut self, direction: ScrollDirection) {
        match direction {
            ScrollDirection::Left => self.move_left(),
            ScrollDirection::Right => self.move_right(),
        };
    }

    pub fn toggle_column_tabbed_display(&mut self) {}

    pub fn set_column_display(&mut self, _display: ColumnDisplay) {}

    pub fn center_column(&mut self) {}

    pub fn center_window(&mut self, _window: Option<&W::Id>) {}

    pub fn center_visible_columns(&mut self) {}

    // ---------------------------------------------------------------------------------------
    // Sizing.

    pub fn toggle_width(&mut self, forwards: bool) {
        self.toggle_preset(None, Axis::Horizontal, forwards);
    }

    pub fn toggle_full_width(&mut self) {}

    pub fn set_window_width(&mut self, window: Option<&W::Id>, change: SizeChange) {
        self.resize_with_change(window, Axis::Horizontal, change);
    }

    pub fn set_window_height(&mut self, window: Option<&W::Id>, change: SizeChange) {
        self.resize_with_change(window, Axis::Vertical, change);
    }

    /// Equalizes the children of the nearest vertical split.
    pub fn reset_window_height(&mut self, window: Option<&W::Id>) {
        let Some(path) = self.target_path(window) else {
            return;
        };
        let Some((k, _)) = self.ancestor_split(&path, Axis::Vertical) else {
            return;
        };
        let split = self.node_mut(&path[..k]).as_split_mut().unwrap();
        let n = split.children.len();
        split.fractions = vec![1. / n as f64; n];
        self.relayout(true, true, None);
    }

    pub fn toggle_window_width(&mut self, window: Option<&W::Id>, forwards: bool) {
        self.toggle_preset(window, Axis::Horizontal, forwards);
    }

    pub fn toggle_window_height(&mut self, window: Option<&W::Id>, forwards: bool) {
        self.toggle_preset(window, Axis::Vertical, forwards);
    }

    pub fn expand_column_to_available_width(&mut self) {}

    pub fn set_fullscreen(&mut self, window: &W::Id, is_fullscreen: bool) -> bool {
        let Some(leaf) = self.leaf_by_window(window) else {
            return false;
        };
        if leaf.is_pending_fullscreen == is_fullscreen {
            return false;
        }

        if self
            .interactive_resize
            .as_ref()
            .is_some_and(|resize| &resize.window == window)
        {
            self.cancel_interactive_resize();
        }

        let leaf = self.leaf_by_window_mut(window).unwrap();
        leaf.is_pending_fullscreen = is_fullscreen;
        self.relayout(true, true, None);
        true
    }

    pub fn set_maximized(&mut self, window: &W::Id, maximize: bool) -> bool {
        let Some(leaf) = self.leaf_by_window(window) else {
            return false;
        };
        if leaf.is_pending_maximized == maximize {
            return false;
        }

        if self
            .interactive_resize
            .as_ref()
            .is_some_and(|resize| &resize.window == window)
        {
            self.cancel_interactive_resize();
        }

        let leaf = self.leaf_by_window_mut(window).unwrap();
        leaf.is_pending_maximized = maximize;
        self.relayout(true, true, None);
        true
    }

    // ---------------------------------------------------------------------------------------
    // Gestures (the dock space has no scrollable view).

    pub fn view_offset_gesture_begin(&mut self, _is_touchpad: bool) {}

    pub fn view_offset_gesture_update(
        &mut self,
        _delta_x: f64,
        _timestamp: Duration,
        _is_touchpad: bool,
    ) -> Option<bool> {
        None
    }

    pub fn view_offset_gesture_end(&mut self, _is_touchpad: Option<bool>) -> bool {
        false
    }

    pub fn dnd_scroll_gesture_begin(&mut self) {}

    pub fn dnd_scroll_gesture_scroll(&mut self, _delta: f64) -> bool {
        false
    }

    pub fn dnd_scroll_gesture_end(&mut self) {}

    // ---------------------------------------------------------------------------------------
    // Interactive resize: dragging an edge moves the divider between the window and its
    // neighbor on that edge.

    pub fn interactive_resize_begin(&mut self, window: W::Id, edges: ResizeEdge) -> bool {
        if self.interactive_resize.is_some() {
            return false;
        }

        let Some(path) = self.path_of_window(&window) else {
            return false;
        };
        if !self.leaf_at(&path).pending_sizing_mode().is_normal() {
            return false;
        }

        let horizontal = edges
            .intersects(ResizeEdge::LEFT_RIGHT)
            .then(|| self.divider_for(&path, Axis::Horizontal, edges.contains(ResizeEdge::LEFT)))
            .flatten();
        let vertical = edges
            .intersects(ResizeEdge::TOP_BOTTOM)
            .then(|| self.divider_for(&path, Axis::Vertical, edges.contains(ResizeEdge::TOP)))
            .flatten();

        self.interactive_resize = Some(DockResize {
            window,
            data: InteractiveResizeData { edges },
            horizontal,
            vertical,
        });

        true
    }

    pub fn interactive_resize_update(
        &mut self,
        window: &W::Id,
        delta: Point<f64, Logical>,
    ) -> bool {
        let Some(resize) = &self.interactive_resize else {
            return false;
        };
        if window != &resize.window {
            return false;
        }

        let edges = resize.data.edges;
        let horizontal = resize.horizontal.clone();
        let vertical = resize.vertical.clone();

        if let Some(drag) = horizontal {
            let dx = if edges.contains(ResizeEdge::LEFT) {
                -delta.x
            } else {
                delta.x
            };
            self.drag_divider(&drag, dx);
        }
        if let Some(drag) = vertical {
            let dy = if edges.contains(ResizeEdge::TOP) {
                -delta.y
            } else {
                delta.y
            };
            self.drag_divider(&drag, dy);
        }

        self.relayout(false, false, None);
        true
    }

    pub fn interactive_resize_end(&mut self, window: Option<&W::Id>) {
        let Some(resize) = &self.interactive_resize else {
            return;
        };

        if let Some(window) = window {
            if window != &resize.window {
                return;
            }
        }

        self.interactive_resize = None;
    }

    pub fn refresh(&mut self, is_active: bool, is_focused: bool) {
        let active = self.active;
        let options = self.options.clone();
        let working_area = self.working_area;
        let resize = self
            .interactive_resize
            .as_ref()
            .map(|resize| (resize.window.clone(), resize.data));

        for leaf in self.leaves_mut() {
            let leaf_id = leaf.id;
            let win = leaf.tile.window_mut();

            win.set_active_in_column(true);
            win.set_floating(false);

            let mut is_active = is_active && Some(leaf_id) == active;
            if options.deactivate_unfocused_windows {
                is_active &= is_focused;
            }
            win.set_activated(is_active);

            let resize_data = resize
                .as_ref()
                .filter(|(id, _)| id == win.id())
                .map(|(_, data)| *data);
            win.set_interactive_resize(resize_data);

            let border_config = options.layout.border.merged_with(&win.rules().border);
            let bounds = compute_toplevel_bounds(
                border_config,
                working_area.size,
                Size::from((0., 0.)),
                options.layout.gaps,
            );
            win.set_bounds(bounds);

            let intent = if options.disable_resize_throttling {
                ConfigureIntent::CanSend
            } else {
                win.configure_intent()
            };

            if matches!(
                intent,
                ConfigureIntent::CanSend | ConfigureIntent::ShouldSend
            ) {
                win.send_pending_configure();
            }

            win.refresh();
        }
    }

    // ---------------------------------------------------------------------------------------
    // Test accessors.

    #[cfg(test)]
    pub fn view_size(&self) -> Size<f64, Logical> {
        self.view_size
    }

    #[cfg(test)]
    pub fn parent_area(&self) -> Rect {
        self.parent_area
    }

    #[cfg(test)]
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    #[cfg(test)]
    pub fn options(&self) -> &Rc<Options> {
        &self.options
    }

    /// Index of the active leaf in tree order (stand-in for the scrolling active column).
    #[cfg(test)]
    pub fn active_column_idx(&self) -> usize {
        let active = self.active;
        self.leaves()
            .iter()
            .position(|leaf| Some(leaf.id) == active)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn view_pos(&self) -> f64 {
        0.
    }

    #[cfg(test)]
    pub(super) fn view_offset(&self) -> NoViewOffset {
        NoViewOffset
    }

    /// Compact description of the tree shape, e.g. `H[1,V[2,3]]`.
    #[cfg(test)]
    pub fn tree_string(&self) -> String {
        fn fmt<W: LayoutElement>(node: &Node<W>, out: &mut String) {
            match node {
                Node::Leaf(leaf) => out.push_str(&format!("{:?}", leaf.tile.window().id())),
                Node::Split(split) => {
                    out.push(match split.axis {
                        Axis::Horizontal => 'H',
                        Axis::Vertical => 'V',
                    });
                    out.push('[');
                    for (idx, child) in split.children.iter().enumerate() {
                        if idx > 0 {
                            out.push(',');
                        }
                        fmt(child, out);
                    }
                    out.push(']');
                }
            }
        }

        let mut out = String::new();
        if let Some(root) = &self.root {
            fmt(root, &mut out);
        }
        out
    }

    /// Layout rectangle of a window's leaf.
    #[cfg(test)]
    pub fn window_rect(&self, window: &W::Id) -> Option<Rect> {
        self.leaf_by_window(window).map(|leaf| leaf.rect)
    }

    /// Leaf id of a window, for building dock targets.
    #[cfg(test)]
    pub fn leaf_id_of(&self, window: &W::Id) -> Option<LeafId> {
        self.leaf_by_window(window).map(|leaf| leaf.id)
    }

    #[cfg(test)]
    pub fn verify_invariants(&self) {
        assert!(self.view_size.w > 0.);
        assert!(self.view_size.h > 0.);
        assert!(self.scale > 0.);
        assert!(self.scale.is_finite());
        assert_eq!(
            self.working_area,
            compute_working_area(self.parent_area, self.scale, self.options.layout.struts)
        );

        assert_eq!(
            self.root.is_none(),
            self.active.is_none(),
            "active leaf must be set if and only if the tree is non-empty"
        );
        if let Some(active) = self.active {
            assert!(
                self.leaf_by_id(active).is_some(),
                "active leaf must be in the tree"
            );
        }

        if let Some(root) = &self.root {
            verify_node(root, true);
        }

        let leaves = self.leaves();
        let mut ids: Vec<_> = leaves.iter().map(|leaf| leaf.id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), leaves.len(), "leaf ids must be unique");

        for leaf in leaves {
            let tile = &leaf.tile;
            assert!(Rc::ptr_eq(&self.options, &tile.options));
            assert_eq!(self.clock, tile.clock);
            assert_eq!(self.scale, tile.scale());
            assert_eq!(self.view_size, tile.view_size());
            assert_eq!(
                leaf.pending_sizing_mode(),
                tile.window().pending_sizing_mode(),
                "leaf pending sizing mode must match the window"
            );
            tile.verify_invariants();
        }

        if let Some(resize) = &self.interactive_resize {
            assert!(
                self.leaf_by_window(&resize.window).is_some(),
                "interactive resize window must be present in the layout"
            );
        }
    }
}

fn render_rank<W: LayoutElement>(leaf: &DockLeaf<W>, active: Option<LeafId>) -> u8 {
    if Some(leaf.id) == active {
        0
    } else if !leaf.tile.sizing_mode().is_normal() {
        1
    } else {
        2
    }
}

fn node_rect<W: LayoutElement>(node: &Node<W>) -> Rect {
    match node {
        Node::Leaf(leaf) => leaf.rect,
        Node::Split(split) => split.rect,
    }
}

/// Leaf of `node` whose far edge (right or bottom along `axis`) is furthest along `axis`.
fn far_edge_leaf<W: LayoutElement>(node: &Node<W>, axis: Axis) -> &DockLeaf<W> {
    let mut leaves = Vec::new();
    node.leaves(&mut leaves);
    let far = |leaf: &&DockLeaf<W>| match axis {
        Axis::Horizontal => leaf.rect.loc.x + leaf.rect.size.w,
        Axis::Vertical => leaf.rect.loc.y + leaf.rect.size.h,
    };
    leaves
        .into_iter()
        .max_by(|a, b| f64::total_cmp(&far(a), &far(b)))
        .unwrap()
}

fn divider_under<W: LayoutElement>(
    node: &Node<W>,
    pos: Point<f64, Logical>,
    pad: f64,
) -> Option<(&DockLeaf<W>, ResizeEdge)> {
    let Node::Split(split) = node else {
        return None;
    };

    let rect = split.rect;
    let (p, perp, perp_lo, perp_hi) = match split.axis {
        Axis::Horizontal => (pos.x, pos.y, rect.loc.y, rect.loc.y + rect.size.h),
        Axis::Vertical => (pos.y, pos.x, rect.loc.x, rect.loc.x + rect.size.w),
    };

    if perp_lo <= perp && perp < perp_hi {
        for (a, b) in zip(&split.children, &split.children[1..]) {
            let (ra, rb) = (node_rect(a), node_rect(b));
            let (end, start) = match split.axis {
                Axis::Horizontal => (ra.loc.x + ra.size.w, rb.loc.x),
                Axis::Vertical => (ra.loc.y + ra.size.h, rb.loc.y),
            };
            if end - pad <= p && p < start + pad {
                let edge = match split.axis {
                    Axis::Horizontal => ResizeEdge::RIGHT,
                    Axis::Vertical => ResizeEdge::BOTTOM,
                };
                return Some((far_edge_leaf(a, split.axis), edge));
            }
        }
    }

    split
        .children
        .iter()
        .find_map(|child| divider_under(child, pos, pad))
}

/// Replaces `node` with a split of `axis` containing the old node and `new`.
fn wrap_in_split<W: LayoutElement>(node: &mut Node<W>, new: Node<W>, axis: Axis, before: bool) {
    let placeholder = Node::Split(Split {
        axis,
        children: Vec::new(),
        fractions: Vec::new(),
        rect: Rect::default(),
    });
    let old = std::mem::replace(node, placeholder);
    let rect = match &old {
        Node::Leaf(leaf) => leaf.rect,
        Node::Split(split) => split.rect,
    };
    let children = if before {
        vec![new, old]
    } else {
        vec![old, new]
    };
    *node = Node::Split(Split {
        axis,
        children,
        fractions: vec![0.5, 0.5],
        rect,
    });
}

#[cfg(test)]
fn verify_node<W: LayoutElement>(node: &Node<W>, is_root: bool) {
    let Node::Split(split) = node else {
        return;
    };

    assert!(
        split.children.len() >= 2,
        "splits must have at least two children"
    );
    assert_eq!(split.children.len(), split.fractions.len());
    let sum: f64 = split.fractions.iter().sum();
    assert!((sum - 1.).abs() < 1e-6, "split fractions must sum to 1");
    for frac in &split.fractions {
        assert!(frac.is_finite() && *frac > 0., "fractions must be positive");
    }

    for child in &split.children {
        if let Node::Split(inner) = child {
            assert_ne!(
                inner.axis, split.axis,
                "nested splits must alternate axes (tree must be normalized)"
            );
        }
        verify_node(child, false);
    }

    let _ = is_root;
}
