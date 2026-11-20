//! Nested docking tree layout.
//!
//! The dock tree replaces niri's scrollable tiling inside a workspace. Every node is either a
//! split (children tiling the split's rectangle along one axis, with gaps between them) or a leaf
//! holding one tile. See `ARBOR.md` for the design.
//!
//! `DockSpace` deliberately exposes the same method names and signatures that `Workspace` used to
//! call on `ScrollingSpace`, so that `workspace.rs` changes stay minimal. Column-specific
//! operations are no-ops or the closest sensible equivalent.

use std::cell::Cell;
use std::cmp::max;
use std::collections::HashMap;
use std::iter::zip;
use std::rc::Rc;
use std::time::Duration;

use niri_config::utils::MergeWith as _;
use niri_config::PresetSize;
use niri_ipc::{ColumnDisplay, SizeChange, WindowLayout};
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Logical, Point, Rectangle, Scale, Serial, Size};

use super::camera::{clamp_to_bounds, pan_to_show, Camera, CameraMode, CameraView};
use super::closing_window::{ClosingWindow, ClosingWindowRenderElement};
use super::icon_tile::{relative_rect, IconTiles, TileContent};
use super::monitor::InsertPosition;
use super::scrolling::{
    compute_toplevel_bounds, compute_working_area, Column, ColumnWidth, ScrollDirection,
};
use super::tab_indicator::{TabIndicator, TabIndicatorRenderElement, TabInfo};
use super::tile::{Tile, TileRenderElement, TileRenderSnapshot};
use super::{ConfigureIntent, HitType, InteractiveResizeData, LayoutElement, Options, RemovedTile};
use crate::animation::{Animation, Clock};
use crate::layout::SizingMode;
use crate::niri_render_elements;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
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

/// Where the middle zone of a docked window starts, relative to its size. Dropping a docked
/// window there joins the tab group.
const TAB_ZONE_START: f64 = 0.25;

/// How long a drag must rest over the middle of a docked window before dropping joins its tab
/// group.
const TAB_HOVER_DELAY: Duration = Duration::from_millis(400);

/// A pause in drag updates longer than this restarts the tab hover (e.g. a new drag).
const TAB_HOVER_GAP: Duration = Duration::from_millis(1000);

/// Smallest thickness of the band around a divider that grabs it (matters when gaps are small).
const DIVIDER_HIT: f64 = 8.;

/// Largest depth of the edge band of a docked window where a window dragged from the Desktop
/// docks beside it.
const LEAF_EDGE_BAND: f64 = 80.;

static LEAF_ID_COUNTER: IdCounter = IdCounter::new();

/// Orders minimizations, so that restoring without a target restores the most recent one.
static MINIMIZE_COUNTER: IdCounter = IdCounter::new();

/// Next minimize order number, shared with minimized floating windows.
pub(super) fn next_minimize_seq() -> u64 {
    MINIMIZE_COUNTER.next()
}

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
    /// Into the tab group of a particular leaf, as the shown tab (the side is ignored).
    Tab(LeafId),
}

/// Where to dock a tile in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockInsert {
    pub target: DockTarget,
    pub side: DockSide,
}

niri_render_elements! {
    DockInnerRenderElement<R> => {
        Tile = TileRenderElement<R>,
        ClosingWindow = ClosingWindowRenderElement,
        TabIndicator = TabIndicatorRenderElement,
    }
}

niri_render_elements! {
    DockSpaceRenderElement<R> => {
        Inner = DockInnerRenderElement<R>,
        Camera = RescaleRenderElement<DockInnerRenderElement<R>>,
        IconTile = PrimaryGpuTextureRenderElement,
    }
}

/// A leaf of the dock tree, holding one tile, or several as a tab group.
///
/// Also acts as a stand-in for a scrolling `Column` where `Workspace` iterates columns.
#[derive(Debug)]
pub struct DockLeaf<W: LayoutElement> {
    id: LeafId,
    /// Tiles of the tab group; never empty. A single tile is a plain leaf.
    tiles: Vec<Tile<W>>,
    /// Index of the shown tab.
    active_tab: usize,
    /// Tab indicator, shown for tab groups.
    tab_indicator: TabIndicator,
    /// Compositor-side fullscreen state of the active tab (changes immediately on request).
    is_pending_fullscreen: bool,
    /// Compositor-side maximized state of the active tab; can be true together with fullscreen,
    /// meaning that unfullscreening goes back to maximized.
    is_pending_maximized: bool,
    /// Set while the leaf is minimized: the window is hidden and its siblings take its space,
    /// but the leaf keeps its slot in the tree. The value orders minimizations.
    minimized: Option<u64>,
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

    /// The camera over the canvas (arbor milestone 2).
    camera: Camera,

    /// Cached icon tile renderings.
    icon_tiles: IconTiles,

    /// A drag resting over the middle of a docked window: the leaf, since when, and when the
    /// drag was last seen there.
    tab_hover: Cell<Option<(LeafId, Duration, Duration)>>,

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
        let tab_indicator = TabIndicator::new(options.layout.tab_indicator);
        tile.update_config(view_size, scale, options);
        let mode = tile.window().pending_sizing_mode();
        Self {
            id: LeafId::next(),
            tiles: vec![tile],
            active_tab: 0,
            tab_indicator,
            is_pending_fullscreen: mode.is_fullscreen(),
            is_pending_maximized: mode.is_maximized(),
            minimized: None,
            rect: Rect::default(),
        }
    }

    pub fn is_minimized(&self) -> bool {
        self.minimized.is_some()
    }

    pub fn is_tabbed(&self) -> bool {
        self.tiles.len() > 1
    }

    /// The shown tile (the active tab).
    fn tile(&self) -> &Tile<W> {
        &self.tiles[self.active_tab]
    }

    fn tile_mut(&mut self) -> &mut Tile<W> {
        &mut self.tiles[self.active_tab]
    }

    fn tab_of(&self, window: &W::Id) -> Option<usize> {
        self.tiles
            .iter()
            .position(|tile| tile.window().id() == window)
    }

    fn tile_for_mut(&mut self, window: &W::Id) -> Option<&mut Tile<W>> {
        self.tiles
            .iter_mut()
            .find(|tile| tile.window().id() == window)
    }

    pub fn contains(&self, window: &W::Id) -> bool {
        self.tab_of(window).is_some()
    }

    pub fn is_pending_fullscreen(&self) -> bool {
        self.is_pending_fullscreen
    }

    pub fn is_pending_maximized(&self) -> bool {
        self.is_pending_maximized
    }

    pub fn tiles(&self) -> impl Iterator<Item = (&Tile<W>, Point<f64, Logical>)> + '_ {
        let loc = self.tiles_rect().loc;
        self.tiles.iter().map(move |tile| (tile, loc))
    }

    /// Whether the tab indicator is shown.
    fn shows_tab_indicator(&self) -> bool {
        self.is_tabbed() && self.minimized.is_none() && self.tile().sizing_mode().is_normal()
    }

    /// Rectangle for the tiles: the leaf rectangle minus room for the tab indicator.
    fn tiles_rect(&self) -> Rect {
        let mut rect = self.rect;
        if self.is_tabbed() {
            let scale = self.tile().scale();
            let extra = self.tab_indicator.extra_size(self.tiles.len(), scale);
            rect.loc += self.tab_indicator.content_offset(self.tiles.len(), scale);
            rect.size.w = f64::max(0., rect.size.w - extra.w);
            rect.size.h = f64::max(0., rect.size.h - extra.h);
        }
        rect
    }

    /// Area of the tab indicator, relative to the tile origin.
    fn tab_indicator_area(&self) -> Rect {
        Rect::from_size(self.tile().animated_tile_size())
    }

    /// Switches to the tab of `window`. Fullscreen and maximize belong to the shown tab, so
    /// switching drops them.
    fn switch_tab(&mut self, window: &W::Id) -> bool {
        let Some(idx) = self.tab_of(window) else {
            return false;
        };
        if idx == self.active_tab {
            return false;
        }
        self.active_tab = idx;
        self.is_pending_fullscreen = false;
        self.is_pending_maximized = false;
        true
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

    /// Whether tab `idx` is positioned on the canvas (and so seen through the camera), rather
    /// than in view coordinates (the shown tab when fullscreen or maximized).
    fn tab_on_canvas(&self, idx: usize) -> bool {
        idx != self.active_tab || self.tile().sizing_mode().is_normal()
    }

    /// Position of tab `idx` in view coordinates through `cam`, including the render offset.
    fn tab_view_pos(&self, idx: usize, parent_area: Rect, cam: CameraView) -> Point<f64, Logical> {
        let pos = self.tab_origin(idx, parent_area) + self.tiles[idx].render_offset();
        if self.tab_on_canvas(idx) {
            cam.to_view(pos)
        } else {
            pos
        }
    }

    /// Position of tab `idx`, not including the render offset.
    fn tab_origin(&self, idx: usize, parent_area: Rect) -> Point<f64, Logical> {
        if idx == self.active_tab {
            self.origin(parent_area)
        } else {
            self.tiles_rect().loc
        }
    }

    /// Position of the shown tile, not including the render offset.
    ///
    /// Follows the tile's *current* sizing mode so that a tile stays in place until it actually
    /// becomes fullscreen or maximized.
    fn origin(&self, parent_area: Rect) -> Point<f64, Logical> {
        match self.tile().sizing_mode() {
            SizingMode::Normal => self.tiles_rect().loc,
            SizingMode::Maximized => parent_area.loc,
            SizingMode::Fullscreen => Point::from((0., 0.)),
        }
    }

    fn request_size(&mut self, parent_area: Rect, animate: bool, transaction: Option<Transaction>) {
        let mode = self.pending_sizing_mode();
        let rect_size = self.tiles_rect().size;
        let active_tab = self.active_tab;
        for (idx, tile) in self.tiles.iter_mut().enumerate() {
            // Hidden tabs stay at the normal size so that switching tabs is instant.
            let mode = if idx == active_tab {
                mode
            } else {
                SizingMode::Normal
            };
            match mode {
                SizingMode::Fullscreen => tile.request_fullscreen(animate, transaction.clone()),
                SizingMode::Maximized => {
                    tile.request_maximized(parent_area.size, animate, transaction.clone())
                }
                SizingMode::Normal => {
                    let mut size = rect_size;
                    let max_size = tile.max_size_nonfullscreen();
                    if max_size.w > 0. {
                        size.w = f64::min(size.w, max_size.w);
                    }
                    if max_size.h > 0. {
                        size.h = f64::min(size.h, max_size.h);
                    }
                    tile.request_tile_size(size, animate, transaction.clone());
                }
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

    /// Whether anything in this subtree is shown (not minimized).
    fn is_visible(&self) -> bool {
        match self {
            Node::Leaf(leaf) => leaf.minimized.is_none(),
            Node::Split(split) => split.children.iter().any(Node::is_visible),
        }
    }

    /// Lays the subtree out in `rect`, keeping leaves at least `min` large where the room
    /// allows (the canvas is sized so that it does).
    fn layout(&mut self, rect: Rect, gaps: f64, scale: f64, min: Size<f64, Logical>) {
        match self {
            Node::Leaf(leaf) => leaf.rect = rect,
            Node::Split(split) => {
                split.rect = rect;

                // Hidden (minimized) children take no space; the visible ones share it.
                let visible: Vec<bool> = split.children.iter().map(Node::is_visible).collect();
                let fractions: Vec<f64> = zip(&split.fractions, &visible)
                    .filter(|(_, v)| **v)
                    .map(|(f, _)| *f)
                    .collect();

                // Children that would get less than they need take what they need, from the
                // others in proportion.
                let n = fractions.len();
                let avail = f64::max(
                    0.,
                    axis_len(rect.size, split.axis) - gaps * n.saturating_sub(1) as f64,
                );
                let fractions = if avail > 0. && (min.w > 0. || min.h > 0.) {
                    let mins: Vec<f64> = zip(&split.children, &visible)
                        .filter(|(_, v)| **v)
                        .map(|(child, _)| {
                            axis_len(required_size(child, min, gaps), split.axis) / avail
                        })
                        .collect();
                    enforce_min_fractions(&fractions, &mins)
                } else {
                    fractions
                };

                let mut rects = split_rects(rect, split.axis, &fractions, gaps, scale).into_iter();
                for (child, visible) in zip(&mut split.children, visible) {
                    let rect = if visible {
                        rects.next().unwrap()
                    } else {
                        Rect::new(rect.loc, Size::default())
                    };
                    child.layout(rect, gaps, scale, min);
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

/// Adjusts fractions so that every child gets at least its minimum share (`mins`, fractions of
/// the available length), taking the room from the other children in proportion.
fn enforce_min_fractions(fractions: &[f64], mins: &[f64]) -> Vec<f64> {
    let n = fractions.len();
    let sum: f64 = fractions.iter().sum();
    let fracs: Vec<f64> = if sum > 0. {
        fractions.iter().map(|f| f / sum).collect()
    } else {
        vec![1. / n as f64; n]
    };

    let total_min: f64 = mins.iter().sum();
    if total_min >= 1. {
        // Not enough room: everyone at their minimum, scaled down.
        return if total_min > 0. {
            mins.iter().map(|m| m / total_min).collect()
        } else {
            fracs
        };
    }

    let mut fixed = vec![false; n];
    let mut out = fracs.clone();
    loop {
        let fixed_total: f64 = (0..n).filter(|&i| fixed[i]).map(|i| mins[i]).sum();
        let free_total: f64 = (0..n).filter(|&i| !fixed[i]).map(|i| fracs[i]).sum();
        let rem = 1. - fixed_total;
        let mut changed = false;
        for i in 0..n {
            if fixed[i] {
                out[i] = mins[i];
            } else {
                out[i] = if free_total > 0. {
                    fracs[i] / free_total * rem
                } else {
                    rem / n as f64
                };
                if out[i] < mins[i] - 1e-12 {
                    fixed[i] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    out
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
    let sum: f64 = fractions.iter().sum();
    let norm = if sum > 0. { sum } else { 1. };

    let mut rv = Vec::with_capacity(n);
    let mut cum = 0.;
    for (i, frac) in fractions.iter().map(|f| f / norm).enumerate() {
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

/// Length available to the visible children of a split (excluding gaps), the number of visible
/// children, and the sum of their fractions.
fn split_avail(split: &Split<impl LayoutElement>, gaps: f64) -> (f64, usize, f64) {
    let mut n: usize = 0;
    let mut sum = 0.;
    for (child, frac) in zip(&split.children, &split.fractions) {
        if child.is_visible() {
            n += 1;
            sum += frac;
        }
    }
    let avail = f64::max(
        0.,
        axis_len(split.rect.size, split.axis) - gaps * n.saturating_sub(1) as f64,
    );
    (avail, n, sum)
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
            tab_hover: Cell::new(None),
            camera: Camera::default(),
            icon_tiles: IconTiles::default(),
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
            for tile in &mut leaf.tiles {
                tile.update_config(view_size, scale, options.clone());
            }
            leaf.tab_indicator
                .update_config(options.layout.tab_indicator);
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
            for tile in &mut leaf.tiles {
                tile.update_shaders();
            }
            leaf.tab_indicator.update_shaders();
        }
    }

    pub fn advance_animations(&mut self) {
        self.camera.advance_animations();
        for leaf in self.leaves_mut() {
            for tile in &mut leaf.tiles {
                tile.advance_animations();
            }
            leaf.tab_indicator.advance_animations();
        }

        self.closing_windows.retain_mut(|closing| {
            closing.advance_animations();
            closing.are_animations_ongoing()
        });
    }

    pub fn are_animations_ongoing(&self) -> bool {
        self.leaves().iter().any(|leaf| {
            leaf.tiles.iter().any(Tile::are_animations_ongoing)
                || leaf.tab_indicator.are_animations_ongoing()
        }) || !self.closing_windows.is_empty()
            || self.is_tab_hover_pending()
            || self.camera.is_animating()
    }

    pub fn are_transitions_ongoing(&self) -> bool {
        self.leaves()
            .iter()
            .any(|leaf| leaf.tiles.iter().any(Tile::are_transitions_ongoing))
            || !self.closing_windows.is_empty()
    }

    pub fn update_render_elements(&mut self, is_active: bool) {
        let active = self.active;
        let parent_area = self.parent_area;
        let view_size = self.view_size;
        let cam = self.camera_view();
        for leaf in self.leaves_mut() {
            let is_active = is_active && Some(leaf.id) == active;
            let mut pos = leaf.origin(parent_area) + leaf.tile().render_offset();
            if leaf.tab_on_canvas(leaf.active_tab) {
                pos -= cam.offset;
            }
            let view_rect = Rect::new(pos.upscale(-1.), view_size);
            let active_tab = leaf.active_tab;
            for (idx, tile) in leaf.tiles.iter_mut().enumerate() {
                tile.update_render_elements(is_active && idx == active_tab, view_rect);
            }

            // The tab indicator, in coordinates relative to the shown tile.
            let enabled = leaf.shows_tab_indicator();
            let area = leaf.tab_indicator_area();
            let config = leaf.tab_indicator.config();
            let scale = leaf.tile().scale();
            let tabs = leaf.tiles.iter().enumerate().map(|(idx, tile)| {
                let is_urgent = tile.window().is_urgent();
                TabInfo::from_tile(
                    tile,
                    Point::default(),
                    idx == active_tab,
                    is_urgent,
                    &config,
                )
            });
            let tabs: Vec<_> = tabs.collect();
            let count = tabs.len();
            leaf.tab_indicator.update_render_elements(
                enabled,
                area,
                view_rect,
                count,
                tabs.into_iter(),
                is_active,
                scale,
            );
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

    /// The active leaf, unless it is minimized (which happens only when every leaf is).
    fn active_leaf(&self) -> Option<&DockLeaf<W>> {
        self.leaf_by_id(self.active?)
            .filter(|leaf| leaf.minimized.is_none())
    }

    fn active_leaf_mut(&mut self) -> Option<&mut DockLeaf<W>> {
        let active = self.active?;
        self.leaves_mut()
            .into_iter()
            .find(|leaf| leaf.id == active)
            .filter(|leaf| leaf.minimized.is_none())
    }

    fn visible_leaves(&self) -> Vec<&DockLeaf<W>> {
        let mut leaves = self.leaves();
        leaves.retain(|leaf| leaf.minimized.is_none());
        leaves
    }

    /// Picks the visible leaf nearest to `rect` as the active leaf, e.g. after the active leaf
    /// went away. Falls back to any leaf so that `active` stays set for a non-empty tree.
    fn activate_nearest(&mut self, rect: Rect) {
        let center = rect_center(rect);
        let nearest = self
            .visible_leaves()
            .into_iter()
            .min_by(|a, b| {
                let da = rect_center(a.rect) - center;
                let db = rect_center(b.rect) - center;
                f64::total_cmp(&da.x.hypot(da.y), &db.x.hypot(db.y))
            })
            .map(|leaf| leaf.id);
        self.active = nearest.or_else(|| self.leaves().first().map(|leaf| leaf.id));
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

    /// The rectangle the tree is laid out in: the root rectangle, grown where leaves would get
    /// smaller than the minimum useful size.
    fn canvas_rect(&self) -> Rect {
        let root = self.root_rect();
        let Some(node) = &self.root else {
            return root;
        };
        let camera = self.options.layout.camera;
        let min = Size::from((camera.min_leaf_width, camera.min_leaf_height));
        let gaps = self.options.layout.gaps;
        let required = required_size(node, min, gaps);
        let size = Size::from((
            round_phys(f64::max(root.size.w, required.w), self.scale),
            round_phys(f64::max(root.size.h, required.h), self.scale),
        ));
        Rect::new(root.loc, size)
    }

    /// The canvas including the outer gaps: what the camera keeps within view.
    fn camera_bounds(&self) -> Rect {
        let gaps = self.options.layout.gaps;
        let canvas = self.canvas_rect();
        Rect::new(
            canvas.loc - Point::from((gaps, gaps)),
            canvas.size + Size::from((gaps * 2., gaps * 2.)),
        )
    }

    /// The current camera position (animated).
    pub fn camera_view(&self) -> CameraView {
        self.camera.current(self.working_area)
    }

    /// Whether the camera shows the canvas scaled (windows are then not interactive).
    pub fn is_zoomed(&self) -> bool {
        self.camera_view().zoom != 1.
    }

    fn camera_anim(&self) -> Animation {
        Animation::new(
            self.clock.clone(),
            0.,
            1.,
            0.,
            self.options.animations.horizontal_view_movement.0,
        )
    }

    /// Path from the root to the active leaf.
    fn active_path(&self) -> Option<Vec<usize>> {
        self.path_of_leaf(self.active?)
    }

    /// Where the camera should be for its mode.
    fn camera_target(&self) -> CameraView {
        let area = self.working_area;
        let bounds = self.camera_bounds();
        let gaps = self.options.layout.gaps;
        let expand = |r: Rect| {
            Rect::new(
                r.loc - Point::from((gaps, gaps)),
                r.size + Size::from((gaps * 2., gaps * 2.)),
            )
        };

        match self.camera.mode {
            CameraMode::Follow => {
                let view = CameraView {
                    offset: self.camera.target.offset,
                    zoom: 1.,
                };
                match self.active_leaf() {
                    Some(leaf) => pan_to_show(view, leaf.rect, gaps, area, bounds),
                    None => clamp_to_bounds(view, area, bounds),
                }
            }
            CameraMode::Framed { depth } => {
                let rect = match self.active_path() {
                    Some(path) => {
                        let depth = depth.min(path.len());
                        node_rect(self.node(&path[..depth]))
                    }
                    None => bounds,
                };
                CameraView::framing(expand(rect), area)
            }
            CameraMode::ShowAll => CameraView::framing(bounds, area),
            CameraMode::Free => self.camera.target,
        }
    }

    /// Moves the camera to where its mode wants it.
    fn update_camera(&mut self, animate: bool) {
        let target = self.camera_target();
        let anim = animate.then(|| self.camera_anim());
        let area = self.working_area;
        self.camera.move_to(target, area, anim);
    }

    /// Switches the camera mode, remembering the previous position for camera-back.
    fn set_camera_mode(&mut self, mode: CameraMode) {
        self.camera.push_history(self.active.map(|id| id.0));
        self.camera.mode = mode;
        self.update_camera(true);
    }

    // Camera actions.

    /// Shows one level more of the tree around the focused window.
    pub fn camera_zoom_out(&mut self) {
        let Some(path) = self.active_path() else {
            return;
        };
        let depth = match self.camera.mode {
            CameraMode::Framed { depth } => depth.min(path.len()),
            CameraMode::ShowAll => return,
            CameraMode::Follow | CameraMode::Free => path.len(),
        };
        if depth == 0 {
            self.set_camera_mode(CameraMode::ShowAll);
            return;
        }

        // Go up until the view actually zooms out (levels that fit at the current zoom would
        // only pan).
        let prev_mode = self.camera.mode;
        let current_zoom = self.camera_view().zoom;
        let mut depth = depth - 1;
        loop {
            self.camera.mode = CameraMode::Framed { depth };
            let target = self.camera_target();
            if target.zoom < current_zoom - 1e-6 || depth == 0 {
                break;
            }
            depth -= 1;
        }
        self.camera.mode = prev_mode;
        let mode = if depth == 0 {
            CameraMode::ShowAll
        } else {
            CameraMode::Framed { depth }
        };
        self.set_camera_mode(mode);
    }

    /// Shows one level less of the tree, toward the focused window; past the window itself,
    /// returns to actual size.
    pub fn camera_zoom_in(&mut self) {
        let Some(path) = self.active_path() else {
            return;
        };
        let mode = match self.camera.mode {
            CameraMode::Framed { depth } if depth < path.len() => {
                CameraMode::Framed { depth: depth + 1 }
            }
            CameraMode::ShowAll => CameraMode::Framed {
                depth: usize::from(!path.is_empty()),
            },
            CameraMode::Free if self.camera.target.zoom < 1. => {
                CameraMode::Framed { depth: path.len() }
            }
            _ => CameraMode::Follow,
        };
        self.set_camera_mode(mode);
    }

    pub fn camera_show_all(&mut self) {
        self.set_camera_mode(CameraMode::ShowAll);
    }

    /// Fits the focused window (or `window`) in view.
    pub fn camera_frame_window(&mut self, window: Option<&W::Id>) {
        if let Some(window) = window {
            if let Some(id) = self.leaf_by_window(window).map(|leaf| leaf.id) {
                self.activate_leaf(id);
            }
        }
        let Some(path) = self.active_path() else {
            return;
        };
        self.set_camera_mode(CameraMode::Framed { depth: path.len() });
    }

    /// Moves focus to the neighbouring window and the camera with it, keeping the zoom level.
    pub fn camera_pan(&mut self, dir: DockSide) {
        let Some(active) = self.active else {
            return;
        };
        let Some(neighbor) = self.neighbor_in_direction(active, dir) else {
            return;
        };
        self.camera.push_history(self.active.map(|id| id.0));
        self.activate_leaf(neighbor);
    }

    pub fn camera_back(&mut self) {
        let focus = self.active.map(|id| id.0);
        if let Some((view, mode, focus)) = self.camera.back(focus) {
            self.apply_camera(view, mode, focus);
        }
    }

    pub fn camera_forward(&mut self) {
        if let Some((view, mode, focus)) = self.camera.forward() {
            self.apply_camera(view, mode, focus);
        }
    }

    fn apply_camera(&mut self, view: CameraView, mode: CameraMode, focus: Option<u64>) {
        // Focus what was focused then, if it is still around and shown.
        if let Some(id) = focus.map(LeafId) {
            if self
                .leaf_by_id(id)
                .is_some_and(|leaf| leaf.minimized.is_none())
            {
                self.active = Some(id);
            }
        }
        self.camera.mode = mode;
        let anim = Some(self.camera_anim());
        let area = self.working_area;
        self.camera.move_to(view, area, anim);
        self.update_camera(true);
    }

    /// Zooms freely around a point in view coordinates (pinch gestures).
    pub fn camera_zoom_by(&mut self, factor: f64, around: Point<f64, Logical>) {
        let current = self.camera_view();
        let zoom = (current.zoom * factor).clamp(0.05, 1.);
        let anchor = current.to_canvas(around);
        let view = CameraView {
            offset: anchor - around.downscale(zoom),
            zoom,
        };
        if self.camera.mode != CameraMode::Free {
            self.camera.push_history(self.active.map(|id| id.0));
        }
        self.camera.mode = if zoom == 1. {
            CameraMode::Follow
        } else {
            CameraMode::Free
        };
        let area = self.working_area;
        self.camera.move_to(view, area, None);
        if zoom == 1. {
            self.update_camera(true);
        }
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

        let rect = self.canvas_rect();
        let gaps = self.options.layout.gaps;
        let scale = self.scale;
        if let Some(root) = &mut self.root {
            let camera = self.options.layout.camera;
            let min = Size::from((camera.min_leaf_width, camera.min_leaf_height));
            root.layout(rect, gaps, scale, min);
        }
        self.update_camera(animate_moves);

        for leaf in self.leaves_mut() {
            // Minimized windows keep their size for when they come back.
            if leaf.minimized.is_some() {
                continue;
            }

            leaf.request_size(parent_area, animate_resize, transaction.clone());

            if animate_moves {
                if let Some(prev) = old.get(&leaf.id) {
                    let delta = *prev - leaf.origin(parent_area);
                    if delta != Point::from((0., 0.)) {
                        for tile in &mut leaf.tiles {
                            tile.animate_move_from(delta);
                        }
                    }
                }
            }
        }
    }

    fn cancel_interactive_resize(&mut self) {
        if let Some(resize) = self.interactive_resize.take() {
            if let Some(leaf) = self.leaf_by_window_mut(&resize.window) {
                if let Some(tile) = leaf.tile_for_mut(&resize.window) {
                    tile.window_mut().cancel_interactive_resize();
                }
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
    ///
    /// Returns the id of the leaf that now holds the tiles (which differs from the inserted
    /// leaf when joining a tab group).
    fn insert_leaf(&mut self, leaf: DockLeaf<W>, mut dock: DockInsert) -> LeafId {
        self.cancel_interactive_resize();

        let Some(root) = self.root.take() else {
            let id = leaf.id;
            self.root = Some(Node::Leaf(Box::new(leaf)));
            return id;
        };
        self.root = Some(root);

        if let DockTarget::Tab(target) = dock.target {
            if let Some(path) = self.path_of_leaf(target) {
                let clock = self.clock.clone();
                let anim = self.options.animations.window_open.anim;
                let group = self.node_mut(&path).as_leaf_mut().unwrap();
                // The incoming tab becomes the shown one, with its sizing mode.
                let at = group.active_tab + 1;
                for (i, tile) in leaf.tiles.into_iter().enumerate() {
                    group.tiles.insert(at + i, tile);
                }
                group.active_tab = at + leaf.active_tab;
                group.is_pending_fullscreen = leaf.is_pending_fullscreen;
                group.is_pending_maximized = leaf.is_pending_maximized;
                group.minimized = None;
                if group.tiles.len() == 2 {
                    group.tab_indicator.start_open_animation(clock, anim);
                }
                return target;
            }

            // Stale target: fall back to the default position.
            dock = self.default_insert();
        }

        let id = leaf.id;
        self.insert_leaf_beside(leaf, dock);
        id
    }

    fn insert_leaf_beside(&mut self, leaf: DockLeaf<W>, dock: DockInsert) {
        let axis = dock.side.axis();
        let before = dock.side.is_before();

        let target_path = match dock.target {
            DockTarget::Root => None,
            DockTarget::Leaf(id) | DockTarget::Tab(id) => match self.path_of_leaf(id) {
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
            self.activate_nearest(leaf.rect);
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
        self.visible_leaves()
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
        let changed = self.active != Some(id);
        self.active = Some(id);
        if let Some(leaf) = self.active_leaf_mut() {
            leaf.tile_mut().ensure_alpha_animates_to_1();
        }
        // Keep the focused window in view.
        if changed {
            self.update_camera(true);
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
        let (avail, n_visible, vis_sum) = split_avail(split, gaps);
        if avail <= 0. || vis_sum <= 0. {
            return;
        }
        let current = split.fractions[idx] / vis_sum * avail;
        let tile = self.leaf_at(path).tile();
        let Some(target) = target_len(current, avail, tile) else {
            return;
        };

        // Work on the visible children only, so that minimized slots keep their share.
        let split = self.node_mut(&path[..k]).as_split_mut().unwrap();
        let visible: Vec<usize> = (0..split.children.len())
            .filter(|&i| split.children[i].is_visible())
            .collect();
        let Some(vis_idx) = visible.iter().position(|&i| i == idx) else {
            return;
        };
        let mut fracs: Vec<f64> = visible
            .iter()
            .map(|&i| split.fractions[i] / vis_sum)
            .collect();
        let min = min_frac(avail, n_visible);
        set_fraction(&mut fracs, vis_idx, target / avail, min);
        for (&i, f) in zip(&visible, fracs) {
            split.fractions[i] = f * vis_sum;
        }
        let n = split.children.len();
        renormalize(&mut split.fractions, n);

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
                (0..idx).rev().find(|&i| split.children[i].is_visible())?
            } else {
                (idx + 1..split.children.len()).find(|&i| split.children[i].is_visible())?
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

        let (avail, n_visible, vis_sum) = split_avail(split, gaps);
        if avail <= 0. || vis_sum <= 0. {
            return;
        }

        let total = drag.orig_frac + drag.orig_neighbor_frac;
        let min = min_frac(avail, n_visible) * vis_sum;
        if total < min * 2. {
            return;
        }

        let frac = (drag.orig_frac + delta / avail * vis_sum).clamp(min, total - min);
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
            // Joining a tab group takes the whole target.
            DockTarget::Tab(id) => match self.leaf_by_id(id) {
                Some(leaf) => leaf.rect,
                None => root_rect,
            },
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
        let mut leaves = self.visible_leaves();
        leaves.sort_by_key(|leaf| render_rank(leaf, active));
        leaves
    }

    // ---------------------------------------------------------------------------------------
    // Queries.

    pub fn tiles(&self) -> impl Iterator<Item = &Tile<W>> + '_ {
        self.leaves().into_iter().flat_map(|leaf| leaf.tiles.iter())
    }

    pub fn tiles_mut(&mut self) -> impl Iterator<Item = &mut Tile<W>> + '_ {
        self.leaves_mut()
            .into_iter()
            .flat_map(|leaf| leaf.tiles.iter_mut())
    }

    /// Leaves of the tree in tree order, standing in for scrolling columns.
    pub fn columns(&self) -> impl Iterator<Item = &DockLeaf<W>> {
        self.leaves().into_iter()
    }

    /// Whether no docked window is shown. Minimized windows can still be in the tree.
    pub fn is_empty(&self) -> bool {
        !self.root.as_ref().is_some_and(Node::is_visible)
    }

    pub fn active_window(&self) -> Option<&W> {
        self.active_leaf().map(|leaf| leaf.tile().window())
    }

    pub fn active_window_mut(&mut self) -> Option<&mut W> {
        self.active_leaf_mut()
            .map(|leaf| leaf.tile_mut().window_mut())
    }

    pub fn active_tile_mut(&mut self) -> Option<&mut Tile<W>> {
        self.active_leaf_mut().map(|leaf| leaf.tile_mut())
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
        if self.is_empty() {
            return InsertPosition::Dock(DockInsert {
                target: DockTarget::Root,
                side: DockSide::Right,
            });
        }

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

        // The rest is about docked windows: work on the canvas.
        let pos = self.camera_view().to_canvas(pos);

        // Resting over the middle of a docked window: join its tab group.
        if let Some(id) = self.tab_zone_target(pos) {
            return InsertPosition::Dock(DockInsert {
                target: DockTarget::Tab(id),
                side: DockSide::Right,
            });
        }

        // Otherwise dock beside the leaf under (or nearest to) the pointer, on the nearest side.
        let leaf = self
            .visible_leaves()
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

    /// The leaf whose middle zone a drag at `pos` has rested over for long enough to join its
    /// tab group. Tracks the rest time across calls.
    fn tab_zone_target(&self, pos: Point<f64, Logical>) -> Option<LeafId> {
        let now = self.clock.now_unadjusted();
        let under = self.visible_leaves().into_iter().find(|leaf| {
            let rect = leaf.rect;
            if !rect.contains(pos) {
                return false;
            }
            let rel_x = (pos.x - rect.loc.x) / rect.size.w;
            let rel_y = (pos.y - rect.loc.y) / rect.size.h;
            let center = |rel: f64| (TAB_ZONE_START..=1. - TAB_ZONE_START).contains(&rel);
            center(rel_x) && center(rel_y)
        });
        let Some(id) = under.map(|leaf| leaf.id) else {
            self.tab_hover.set(None);
            return None;
        };

        let since = match self.tab_hover.get() {
            Some((prev, since, last))
                if prev == id
                    && last <= now
                    && since <= now
                    && now.saturating_sub(last) <= TAB_HOVER_GAP =>
            {
                since
            }
            _ => now,
        };
        self.tab_hover.set(Some((id, since, now)));
        (now.saturating_sub(since) >= TAB_HOVER_DELAY).then_some(id)
    }

    /// Whether a drag is resting over the middle of a docked window, still waiting to become a
    /// tab target (frames must keep coming so that the hint updates).
    fn is_tab_hover_pending(&self) -> bool {
        let now = self.clock.now_unadjusted();
        self.tab_hover.get().is_some_and(|(_, since, last)| {
            now.saturating_sub(last) <= TAB_HOVER_GAP && now.saturating_sub(since) < TAB_HOVER_DELAY
        })
    }

    /// Nodes that are seen narrower than the icon threshold (outermost first), when zoomed out.
    fn collapsed_nodes(&self, zoom: f64) -> Vec<CollapsedNode> {
        let threshold = self.options.layout.camera.icon_threshold;
        let mut out = Vec::new();
        if zoom >= 1. || threshold <= 0. {
            return out;
        }
        if let Some(root) = &self.root {
            collect_collapsed(root, zoom, threshold, self.active, &mut out);
        }
        out
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

        let cam = self.camera_view();
        let pos = cam.to_canvas(pos);
        let gaps = self.options.layout.gaps;
        // The grab band is in screen pixels.
        let pad = f64::max(0., (DIVIDER_HIT / cam.zoom - gaps) / 2.);
        divider_under(root, pos, pad).map(|(leaf, edge)| (leaf.tile().window().id().clone(), edge))
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

        // The rest is about docked windows: work on the canvas.
        let pos = self.camera_view().to_canvas(pos);

        // Resting over the middle of a docked window: join its tab group.
        if let Some(id) = self.tab_zone_target(pos) {
            return InsertPosition::Dock(DockInsert {
                target: DockTarget::Tab(id),
                side: DockSide::Right,
            });
        }

        // Over the edge band of a docked window: dock beside it.
        for leaf in self.visible_leaves() {
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
            InsertPosition::Dock(dock) => {
                Some(self.camera_view().rect_to_view(self.hint_rect(dock)))
            }
            InsertPosition::NewColumn(_) | InsertPosition::InColumn(_, _) => Some(
                self.camera_view()
                    .rect_to_view(self.hint_rect(self.default_insert())),
            ),
            InsertPosition::Floating => None,
        }
    }

    pub fn tiles_with_render_positions(
        &self,
    ) -> impl Iterator<Item = (&Tile<W>, Point<f64, Logical>, bool)> {
        let scale = self.scale;
        let parent_area = self.parent_area;
        let cam = self.camera_view();
        let minimized = self.leaves().into_iter().filter(|leaf| leaf.is_minimized());
        self.leaves_in_render_order()
            .into_iter()
            .chain(minimized)
            .flat_map(move |leaf| {
                // The shown tab first.
                let order = std::iter::once(leaf.active_tab)
                    .chain((0..leaf.tiles.len()).filter(move |idx| *idx != leaf.active_tab));
                order.map(move |idx| {
                    let tile = &leaf.tiles[idx];
                    let pos = leaf.tab_view_pos(idx, parent_area, cam);
                    // Round to physical pixels.
                    let pos = pos.to_physical_precise_round(scale).to_logical(scale);
                    let visible = !leaf.is_minimized() && idx == leaf.active_tab;
                    (tile, pos, visible)
                })
            })
    }

    pub fn tiles_with_render_positions_mut(
        &mut self,
        round: bool,
    ) -> impl Iterator<Item = (&mut Tile<W>, Point<f64, Logical>)> {
        let scale = self.scale;
        let parent_area = self.parent_area;
        let active = self.active;
        let cam = self.camera_view();
        let mut leaves = self.leaves_mut();
        leaves.sort_by_key(|leaf| render_rank(leaf, active));
        leaves.into_iter().flat_map(move |leaf| {
            let active_tab = leaf.active_tab;
            let origin = leaf.origin(parent_area);
            let tabs_loc = leaf.tiles_rect().loc;
            let active_on_canvas = leaf.tab_on_canvas(active_tab);
            leaf.tiles.iter_mut().enumerate().map(move |(idx, tile)| {
                let loc = if idx == active_tab { origin } else { tabs_loc };
                let mut pos = loc + tile.render_offset();
                if idx != active_tab || active_on_canvas {
                    pos = cam.to_view(pos);
                }
                if round {
                    pos = pos.to_physical_precise_round(scale).to_logical(scale);
                }
                (tile, pos)
            })
        })
    }

    pub fn tiles_with_ipc_layouts(&self) -> impl Iterator<Item = (&Tile<W>, WindowLayout)> {
        let scale = self.scale;
        let parent_area = self.parent_area;
        let cam = self.camera_view();
        self.leaves().into_iter().flat_map(move |leaf| {
            leaf.tiles.iter().enumerate().map(move |(idx, tile)| {
                let mut pos = leaf.tab_origin(idx, parent_area);
                if leaf.tab_on_canvas(idx) {
                    pos = cam.to_view(pos);
                }
                let pos = pos.to_physical_precise_round(scale).to_logical(scale);
                let layout = WindowLayout {
                    pos_in_scrolling_layout: None,
                    tile_pos_in_workspace_view: Some(pos.into()),
                    ..tile.ipc_layout_template()
                };
                (tile, layout)
            })
        })
    }

    /// Returns the geometry of the active tile relative to and clamped to the view.
    pub fn active_tile_visual_rectangle(&self) -> Option<Rect> {
        let leaf = self.active_leaf()?;
        let mut rect = Rect::new(leaf.origin(self.parent_area), leaf.tile().tile_size());
        if leaf.tab_on_canvas(leaf.active_tab) {
            rect = self.camera_view().rect_to_view(rect);
        }
        Rect::from_size(self.view_size).intersection(rect)
    }

    pub fn popup_target_rect(&self, id: &W::Id) -> Option<Rect> {
        let leaf = self.leaf_by_window(id)?;
        let idx = leaf.tab_of(id)?;
        let mut target = self.parent_area;
        let mut origin = leaf.tab_origin(idx, self.parent_area);
        if leaf.tab_on_canvas(idx) {
            origin -= self.camera_view().offset;
        }
        target.loc -= origin;
        target.loc -= leaf.tiles[idx].window_loc();
        Some(target)
    }

    pub fn render_above_top_layer(&self) -> bool {
        self.active_leaf()
            .is_some_and(|leaf| leaf.tile().sizing_mode().is_fullscreen())
    }

    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<(&W, HitType)> {
        let cam = self.camera_view();

        // The tab indicators of tab groups.
        for leaf in self.leaves_in_render_order() {
            if !leaf.shows_tab_indicator() {
                continue;
            }
            let tile_pos = leaf.tab_view_pos(leaf.active_tab, self.parent_area, cam);
            let tile_pos = tile_pos
                .to_physical_precise_round(self.scale)
                .to_logical(self.scale);
            // The indicator only shows for normal tiles, which go through the camera.
            if let Some(idx) = leaf.tab_indicator.hit(
                leaf.tab_indicator_area(),
                leaf.tiles.len(),
                self.scale,
                (pos - tile_pos).downscale(cam.zoom),
            ) {
                let hit = HitType::Activate {
                    is_tab_indicator: true,
                };
                return Some((leaf.tiles[idx].window(), hit));
            }
        }

        // This matches self.tiles_with_render_positions().
        if cam.zoom == 1. {
            return self
                .tiles_with_render_positions()
                .filter(|(_, _, visible)| *visible)
                .find_map(|(tile, tile_pos, _)| HitType::hit_tile(tile, tile_pos, pos));
        }

        // Zoomed: windows on the canvas are not interactive, only activatable.
        let hit = self
            .tiles_with_render_positions()
            .filter(|(_, _, visible)| *visible)
            .find_map(|(tile, tile_pos, _)| {
                if tile.sizing_mode().is_normal() {
                    tile.hit((pos - tile_pos).downscale(cam.zoom))
                        .map(|hit| (tile.window(), hit.to_activate()))
                } else {
                    HitType::hit_tile(tile, tile_pos, pos)
                }
            });
        if hit.is_some() {
            return hit;
        }

        // Anywhere in a window's slot (icon tiles cover the slot) activates it.
        let canvas_pos = cam.to_canvas(pos);
        self.visible_leaves()
            .into_iter()
            .find(|leaf| leaf.rect.contains(canvas_pos))
            .map(|leaf| {
                let hit = HitType::Activate {
                    is_tab_indicator: false,
                };
                (leaf.tile().window(), hit)
            })
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
            push(DockSpaceRenderElement::Inner(elem.into()));
        }

        let cam = self.camera_view();
        let zoom = cam.zoom;
        let active = self.active;
        let parent_area = self.parent_area;

        // Nodes seen too small to be useful draw as icon tiles instead.
        let collapsed = self.collapsed_nodes(zoom);
        let mut hidden_leaves = Vec::new();
        for node in &collapsed {
            hidden_leaves.extend_from_slice(&node.leaves);
            let rect = cam.rect_to_view(node.rect);
            let rect = Rect::new(
                rect.loc
                    .to_physical_precise_round(self.scale)
                    .to_logical(self.scale),
                rect.size,
            );
            if let Some(elem) = self.icon_tiles.render(
                ctx.renderer,
                node.id,
                rect,
                self.scale,
                node.content.clone(),
                focus_ring && node.leaves.contains(&active.unwrap_or(LeafId(u64::MAX))),
            ) {
                push(DockSpaceRenderElement::IconTile(elem));
            }
        }
        self.icon_tiles
            .retain(|id| collapsed.iter().any(|node| node.id == id));

        for leaf in self.leaves_in_render_order() {
            if hidden_leaves.contains(&leaf.id) && leaf.tile().sizing_mode().is_normal() {
                continue;
            }
            // Normal tiles are on the canvas and go through the camera: render them at
            // (canvas - offset) and scale around the origin. Fullscreen and maximized tiles are
            // in view coordinates already.
            let through_camera = leaf.tile().sizing_mode().is_normal();
            let mut push_elem = |elem: DockInnerRenderElement<R>| {
                if through_camera && zoom != 1. {
                    let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), zoom);
                    push(DockSpaceRenderElement::Camera(elem));
                } else {
                    push(DockSpaceRenderElement::Inner(elem));
                }
            };

            let mut tile_pos = leaf.origin(parent_area) + leaf.tile().render_offset();
            if through_camera {
                tile_pos -= cam.offset;
            }
            let tile_pos = tile_pos
                .to_physical_precise_round(self.scale)
                .to_logical(self.scale);

            // The tab indicator goes on top of the tile.
            leaf.tab_indicator
                .render(ctx.renderer, tile_pos, &mut |elem| push_elem(elem.into()));

            let focus_ring = focus_ring && Some(leaf.id) == active;
            let xray_pos = xray_pos.offset(tile_pos);
            leaf.tile()
                .render(ctx.r(), tile_pos, xray_pos, focus_ring, &mut |elem| {
                    push_elem(elem.into())
                });
        }
    }

    // ---------------------------------------------------------------------------------------
    // Adding and removing.

    /// Docks a tile at the given position in the tree.
    pub fn add_tile_docked(&mut self, tile: Tile<W>, dock: DockInsert, activate: bool) {
        let leaf = DockLeaf::new(tile, self.view_size, self.scale, self.options.clone());
        let id = self.insert_leaf(leaf, dock);
        if activate || self.active_leaf().is_none() {
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
            let id = self.insert_leaf(leaf, dock);
            if (activate && prev.is_none()) || self.active_leaf().is_none() {
                self.activate_leaf(id);
            }
            prev = Some(id);
        }
        self.relayout(true, true, None);
    }

    pub fn remove_active_tile(&mut self, transaction: Transaction) -> Option<RemovedTile<W>> {
        let id = self.active_leaf()?.tile().window().id().clone();
        Some(self.remove_tile(&id, transaction))
    }

    pub fn remove_tile(&mut self, window: &W::Id, transaction: Transaction) -> RemovedTile<W> {
        let path = self
            .path_of_window(window)
            .expect("window must be in the dock tree");

        let mut tile = if self.leaf_at(&path).is_tabbed() {
            // Take the tab out of its group; the leaf stays.
            if self
                .interactive_resize
                .as_ref()
                .is_some_and(|resize| self.leaf_at(&path).contains(&resize.window))
            {
                self.cancel_interactive_resize();
            }

            let leaf = self.node_mut(&path).as_leaf_mut().unwrap();
            let idx = leaf.tab_of(window).unwrap();
            let tile = leaf.tiles.remove(idx);
            if idx == leaf.active_tab {
                // Fullscreen and maximize belonged to the removed tab.
                leaf.is_pending_fullscreen = false;
                leaf.is_pending_maximized = false;
                leaf.active_tab = idx.min(leaf.tiles.len() - 1);
            } else if idx < leaf.active_tab {
                leaf.active_tab -= 1;
            }
            tile
        } else {
            self.take_leaf(&path).tiles.pop().unwrap()
        };
        self.relayout(true, true, Some(transaction));

        tile.window_mut().set_minimized(false);

        RemovedTile {
            tile,
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
        let idx = leaf.tab_of(window).unwrap();

        let prev_origin = leaf.tab_origin(idx, parent_area);

        // Do this before calling update_window() so it can get up-to-date info.
        if let Some(serial) = serial {
            leaf.tiles[idx].window_mut().on_commit(serial);
        }

        leaf.tiles[idx].update_window();

        // Going in and out of fullscreen/maximized moves the tile; animate that.
        let delta = prev_origin - leaf.tab_origin(idx, parent_area);
        if delta != Point::from((0., 0.)) {
            leaf.tiles[idx].animate_move_from(delta);
        }
    }

    /// Activates the window, restoring it first if it is minimized.
    pub fn activate_window(&mut self, window: &W::Id) -> bool {
        let Some(id) = self.leaf_by_window(window).map(|leaf| leaf.id) else {
            return false;
        };
        self.restore_window(window);
        self.switch_tab(window);
        self.activate_leaf(id);
        true
    }

    // ---------------------------------------------------------------------------------------
    // Tab groups.

    /// Shows the tab of `window` in its tab group. Does not activate the leaf.
    fn switch_tab(&mut self, window: &W::Id) -> bool {
        let resizing_group = self.interactive_resize.as_ref().is_some_and(|resize| {
            self.leaf_by_window(window)
                .is_some_and(|leaf| leaf.contains(&resize.window))
        });

        let Some(leaf) = self.leaf_by_window_mut(window) else {
            return false;
        };
        if !leaf.switch_tab(window) {
            return false;
        }

        if resizing_group {
            self.cancel_interactive_resize();
        }
        self.relayout(false, false, None);
        true
    }

    /// Shows the next (or previous) tab of the active tab group, wrapping around.
    pub fn focus_tab(&mut self, forwards: bool) -> bool {
        let Some(leaf) = self.active_leaf() else {
            return false;
        };
        let n = leaf.tiles.len();
        if n < 2 {
            return false;
        }
        let idx = if forwards {
            (leaf.active_tab + 1) % n
        } else {
            (leaf.active_tab + n - 1) % n
        };
        let window = leaf.tiles[idx].window().id().clone();
        self.switch_tab(&window)
    }

    // ---------------------------------------------------------------------------------------
    // Minimize.

    pub fn is_minimized(&self, window: &W::Id) -> bool {
        self.leaf_by_window(window)
            .is_some_and(|leaf| leaf.is_minimized())
    }

    /// Hides the window; its siblings take its space, and its slot stays in the tree.
    pub fn minimize_window(&mut self, window: &W::Id) -> bool {
        let Some(leaf) = self.leaf_by_window(window) else {
            return false;
        };
        if leaf.is_minimized() {
            return false;
        }
        let (id, rect) = (leaf.id, leaf.rect);

        if self
            .interactive_resize
            .as_ref()
            .is_some_and(|resize| &resize.window == window)
        {
            self.cancel_interactive_resize();
        }

        let leaf = self.leaf_by_window_mut(window).unwrap();
        leaf.minimized = Some(MINIMIZE_COUNTER.next());

        if self.active == Some(id) {
            self.activate_nearest(rect);
        }

        self.relayout(true, true, None);
        true
    }

    /// Puts a minimized window back into its slot. Does not activate it.
    pub fn restore_window(&mut self, window: &W::Id) -> bool {
        let Some(leaf) = self.leaf_by_window_mut(window) else {
            return false;
        };
        if leaf.minimized.take().is_none() {
            return false;
        }
        let id = leaf.id;

        // With every other leaf minimized, the active leaf was a minimized one; take over.
        if self.active_leaf().is_none() {
            self.active = Some(id);
        }

        self.relayout(true, true, None);
        true
    }

    /// The most recently minimized window and its minimize order.
    pub fn last_minimized_window(&self) -> Option<(u64, &W::Id)> {
        self.leaves()
            .into_iter()
            .filter_map(|leaf| leaf.minimized.map(|seq| (seq, leaf)))
            .max_by_key(|(seq, _)| *seq)
            .map(|(seq, leaf)| (seq, leaf.tile().window().id()))
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
                leaf.tile_for_mut(id).unwrap().start_open_animation();
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
        if let Some(id) = self.visible_leaves().first().map(|leaf| leaf.id) {
            self.activate_leaf(id);
        }
    }

    pub fn focus_column_last(&mut self) {
        if let Some(id) = self.visible_leaves().last().map(|leaf| leaf.id) {
            self.activate_leaf(id);
        }
    }

    /// Focuses the leaf with this 1-based index in tree order.
    pub fn focus_column(&mut self, index: usize) {
        let leaves = self.visible_leaves();
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
        // A hidden tab is never fullscreen or maximized; going there shows it first.
        if leaf.tile().window().id() != window {
            if !is_fullscreen {
                return false;
            }
            self.switch_tab(window);
        }
        let leaf = self.leaf_by_window(window).unwrap();
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
        // A hidden tab is never fullscreen or maximized; going there shows it first.
        if leaf.tile().window().id() != window {
            if !maximize {
                return false;
            }
            self.switch_tab(window);
        }
        let leaf = self.leaf_by_window(window).unwrap();
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
        let leaf = self.leaf_at(&path);
        if !leaf.pending_sizing_mode().is_normal() || leaf.is_minimized() {
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
        // The pointer moves in view pixels; dividers live on the canvas.
        let delta = delta.downscale(self.camera_view().zoom);

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
            let is_minimized = leaf.minimized.is_some();
            let active_tab = leaf.active_tab;
            for (idx, tile) in leaf.tiles.iter_mut().enumerate() {
                let is_shown_tab = idx == active_tab;
                let win = tile.window_mut();

                win.set_active_in_column(is_shown_tab);
                win.set_floating(false);

                let mut is_active =
                    is_active && Some(leaf_id) == active && !is_minimized && is_shown_tab;
                if options.deactivate_unfocused_windows {
                    is_active &= is_focused;
                }
                win.set_activated(is_active);
                win.set_minimized(is_minimized);

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
    }

    // ---------------------------------------------------------------------------------------
    // Test accessors.

    /// Leaf counts of the nodes drawn as icon tiles, outermost first.
    #[cfg(test)]
    pub fn icon_tile_leaf_counts(&self) -> Vec<usize> {
        self.collapsed_nodes(self.camera_view().zoom)
            .iter()
            .map(|node| node.leaves.len())
            .collect()
    }

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
                Node::Leaf(leaf) if leaf.is_tabbed() => {
                    out.push_str("T[");
                    for (idx, tile) in leaf.tiles.iter().enumerate() {
                        if idx > 0 {
                            out.push(',');
                        }
                        if idx == leaf.active_tab {
                            out.push('*');
                        }
                        out.push_str(&format!("{:?}", tile.window().id()));
                    }
                    out.push(']');
                }
                Node::Leaf(leaf) => out.push_str(&format!("{:?}", leaf.tile().window().id())),
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
            let leaf = self.leaf_by_id(active);
            assert!(leaf.is_some(), "active leaf must be in the tree");
            if leaf.unwrap().is_minimized() {
                assert!(
                    self.visible_leaves().is_empty(),
                    "active leaf can be minimized only if every leaf is"
                );
            }
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
            assert!(!leaf.tiles.is_empty(), "leaves must hold at least one tile");
            assert!(leaf.active_tab < leaf.tiles.len());
            for (idx, tile) in leaf.tiles.iter().enumerate() {
                assert!(Rc::ptr_eq(&self.options, &tile.options));
                assert_eq!(self.clock, tile.clock);
                assert_eq!(self.scale, tile.scale());
                assert_eq!(self.view_size, tile.view_size());
                let expected = if idx == leaf.active_tab {
                    leaf.pending_sizing_mode()
                } else {
                    SizingMode::Normal
                };
                assert_eq!(
                    expected,
                    tile.window().pending_sizing_mode(),
                    "the shown tab's pending sizing mode must match the leaf; \
                     hidden tabs must be normal"
                );
                tile.verify_invariants();
            }
        }

        let cam = self.camera_view();
        assert!(cam.zoom.is_finite() && cam.zoom > 0. && cam.zoom <= 1. + 1e-9);
        assert!(cam.offset.x.is_finite() && cam.offset.y.is_finite());

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
    } else if !leaf.tile().sizing_mode().is_normal() {
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

/// Size a subtree needs so that no visible leaf is smaller than `min`, given its fractions.
fn required_size<W: LayoutElement>(
    node: &Node<W>,
    min: Size<f64, Logical>,
    gaps: f64,
) -> Size<f64, Logical> {
    match node {
        Node::Leaf(leaf) if leaf.minimized.is_some() => Size::default(),
        Node::Leaf(_) => min,
        Node::Split(split) => {
            let visible: Vec<(Size<f64, Logical>, f64)> = zip(&split.children, &split.fractions)
                .filter(|(child, _)| child.is_visible())
                .map(|(child, frac)| (required_size(child, min, gaps), *frac))
                .collect();
            if visible.is_empty() {
                return Size::default();
            }
            let n_gaps = gaps * (visible.len() - 1) as f64;
            // Along the axis the children need their lengths side by side (the layout gives
            // children below their minimum extra room from the others).
            let along = visible
                .iter()
                .map(|(size, _)| axis_len(*size, split.axis))
                .sum::<f64>()
                + n_gaps;
            let across = visible
                .iter()
                .map(|(size, _)| match split.axis {
                    Axis::Horizontal => size.h,
                    Axis::Vertical => size.w,
                })
                .fold(0., f64::max);
            match split.axis {
                Axis::Horizontal => Size::from((along, across)),
                Axis::Vertical => Size::from((across, along)),
            }
        }
    }
}

/// A node drawn as one icon tile.
struct CollapsedNode {
    /// Cache id of the tile.
    id: u64,
    /// Canvas rectangle.
    rect: Rect,
    content: TileContent,
    /// Leaves inside (not drawn themselves).
    leaves: Vec<LeafId>,
}

fn leaf_app_id<W: LayoutElement>(leaf: &DockLeaf<W>) -> String {
    leaf.tile().window().app_id().unwrap_or_default()
}

fn collect_collapsed<W: LayoutElement>(
    node: &Node<W>,
    zoom: f64,
    threshold: f64,
    active: Option<LeafId>,
    out: &mut Vec<CollapsedNode>,
) {
    if !node.is_visible() {
        return;
    }
    let rect = node_rect(node);
    let small = rect.size.w * zoom < threshold || rect.size.h * zoom < threshold / 2.;
    match node {
        Node::Leaf(leaf) => {
            if small && leaf.tile().sizing_mode().is_normal() {
                out.push(CollapsedNode {
                    id: leaf.id.0,
                    rect,
                    content: TileContent::App {
                        app_id: leaf_app_id(leaf),
                        has_icon: false,
                    },
                    leaves: vec![leaf.id],
                });
            }
        }
        Node::Split(split) => {
            if small {
                let mut leaves = Vec::new();
                node.leaves(&mut leaves);
                leaves.retain(|leaf| leaf.minimized.is_none());
                if leaves
                    .iter()
                    .any(|leaf| !leaf.tile().sizing_mode().is_normal())
                {
                    // Don't hide fullscreen or maximized windows.
                } else {
                    let shown = leaves
                        .iter()
                        .find(|leaf| Some(leaf.id) == active)
                        .or(leaves.first())
                        .copied();
                    out.push(CollapsedNode {
                        id: leaves.first().map_or(0, |leaf| leaf.id.0) | (1 << 63),
                        rect,
                        content: TileContent::Group {
                            leaves: leaves
                                .iter()
                                .map(|leaf| relative_rect(rect, leaf.rect))
                                .collect(),
                            app_id: shown.map(leaf_app_id).unwrap_or_default(),
                        },
                        leaves: leaves.iter().map(|leaf| leaf.id).collect(),
                    });
                    return;
                }
            }
            for child in &split.children {
                collect_collapsed(child, zoom, threshold, active, out);
            }
        }
    }
}

/// Leaf of `node` whose far edge (right or bottom along `axis`) is furthest along `axis`.
fn far_edge_leaf<W: LayoutElement>(node: &Node<W>, axis: Axis) -> &DockLeaf<W> {
    let mut leaves = Vec::new();
    node.leaves(&mut leaves);
    leaves.retain(|leaf| leaf.minimized.is_none());
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
        let visible: Vec<&Node<W>> = split.children.iter().filter(|c| c.is_visible()).collect();
        for pair in visible.windows(2) {
            let (a, b) = (pair[0], pair[1]);
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
