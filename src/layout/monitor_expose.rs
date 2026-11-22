//! arbor: the Mission Control-style overview on one monitor: geometry, rendering and hit
//! testing. Included from `monitor.rs` so it can reach the monitor's private state; the layout
//! math is in `expose.rs`.
//!
//! While the overview is open (or animating), the monitor draws this instead of its workspaces:
//! every visible window of the shown workspace moves from its real place to its exposé slot as
//! the overview progress goes from 0 to 1 (the same progress the touchpad swipe scrubs), with
//! its title and app icon under it, a hover outline and close button, a bar of workspace
//! thumbnails along the top (the workspace's real windows, live and scaled down, with each
//! app's icon in front), and the wallpaper dimmed behind.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use arbor_config::{Color, CornerRadius, GradientInterpolation};
use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::{Alignment, EllipsizeMode, FontDescription, Weight};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{
    CropRenderElement, Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use super::{Monitor, MonitorInnerRenderElement, MonitorRenderElement};
use crate::layout::expose::{self, lerp, ExposeHit, ExposeParams, ExposeSlot};
use crate::layout::icon_tile::{app_color, app_icon, app_letter, paint_icon, rounded_rect};
use crate::layout::tile::Tile;
use crate::layout::workspace::{Workspace, WorkspaceId};
use crate::layout::LayoutElement;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::memory::MemoryBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::ArborRenderer;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::RenderCtx;
use crate::utils::to_physical_precise_round;

type Rect = Rectangle<f64, Logical>;

/// Gap between windows and between rows.
const GAP: f64 = 32.;
/// Label strip under each window (title and icon).
const LABEL_HEIGHT: f64 = 34.;
/// Margin left and right of the exposé and the bar.
const SIDE_MARGIN: f64 = 48.;
/// Space above and below the workspace bar.
const BAR_PADDING: f64 = 16.;
/// Space below the exposé.
const BOTTOM_MARGIN: f64 = 24.;
/// Gap between workspace thumbnails.
const BAR_GAP: f64 = 16.;
/// Size of an app icon in a label.
const LABEL_ICON: f64 = 20.;
/// Radius of a window's close button.
const CLOSE_RADIUS: f64 = 12.;
/// Outline around the hovered window.
const OUTLINE_WIDTH: f64 = 3.;
const OUTLINE_PAD: f64 = 6.;
/// Accent colour (Breeze highlight).
const ACCENT: (f64, f64, f64) = (0.239, 0.682, 0.914);
/// Darkening of the wallpaper behind the open overview.
const DIM_ALPHA: f32 = 0.55;

/// A window being dragged in the exposé.
#[derive(Debug)]
struct ExposeDrag<Id> {
    window: Id,
    /// Pointer position within the output.
    pos: Point<f64, Logical>,
    /// Where the window was grabbed, relative to its slot (0..1).
    grab: Point<f64, Logical>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TexKey {
    Label {
        title: String,
        app_id: String,
        has_icon: bool,
        width: i32,
        scale: u64,
    },
    Close {
        scale: u64,
    },
    Thumb(ThumbKey),
}

/// A window in a workspace thumbnail: rectangle relative to the thumbnail (in 1/10000), app id,
/// whether its icon is loaded.
type ThumbWindow = ((i32, i32, i32, i32), String, bool);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThumbKey {
    /// The layer under the live windows (the background) rather than the
    /// one over them (app icons, name, border).
    back: bool,
    size: (i32, i32),
    scale: u64,
    name: String,
    is_active: bool,
    is_hovered: bool,
    windows: Vec<ThumbWindow>,
}

struct CachedTex {
    buffer: Option<MemoryBuffer>,
    texture: Option<TextureBuffer<GlesTexture>>,
    used: bool,
}

/// Per-monitor exposé state: pointer, drag, and cached textures.
pub(super) struct ExposeUi<Id> {
    pointer: Option<Point<f64, Logical>>,
    drag: Option<ExposeDrag<Id>>,
    textures: RefCell<HashMap<TexKey, CachedTex>>,
    /// Textures being rasterized on a background thread.
    in_flight: RefCell<HashSet<TexKey>>,
    /// Rasterized textures handed back by those threads, taken at the next render.
    drawn: Arc<Mutex<Vec<(TexKey, Option<MemoryBuffer>, std::time::Duration)>>>,
    /// Outline elements (GPU-drawn, so they resize with the animation for free), reused across
    /// frames to keep their ids stable for damage tracking.
    outlines: RefCell<Vec<BorderRenderElement>>,
    /// The texture last shown in each workspace thumbnail's place (by workspace index). While a
    /// thumbnail's new look (hovered, a window moved...) is still being drawn, the old one stays
    /// on screen instead of the thumbnail blinking out for a frame or two.
    shown_thumbs: RefCell<HashMap<(usize, bool), TexKey>>,
    /// Each workspace's window slots, laid out once when the overview opens (and again only
    /// when its windows or the area change). Focusing a window raises it and moves the camera
    /// to it; laying the windows out again from those new places would swap their slots, so a
    /// different window would fly forward before the picked one snapped into place.
    pub(super) slots: RefCell<HashMap<WorkspaceId, (Rect, Vec<(Id, ExposeSlot)>)>>,
    dim: RefCell<SolidColorBuffer>,
}

impl<Id> Default for ExposeUi<Id> {
    fn default() -> Self {
        Self {
            pointer: None,
            drag: None,
            textures: RefCell::new(HashMap::new()),
            in_flight: RefCell::new(HashSet::new()),
            drawn: Arc::new(Mutex::new(Vec::new())),
            outlines: RefCell::new(Vec::new()),
            shown_thumbs: RefCell::new(HashMap::new()),
            slots: RefCell::new(HashMap::new()),
            dim: RefCell::new(SolidColorBuffer::new((0., 0.), [0., 0., 0., 1.])),
        }
    }
}

impl<Id: std::fmt::Debug> std::fmt::Debug for ExposeUi<Id> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExposeUi")
            .field("pointer", &self.pointer)
            .field("drag", &self.drag)
            .finish_non_exhaustive()
    }
}

/// A window in the exposé: where it really is and where its slot is.
struct Item<'a, W: LayoutElement> {
    tile: &'a Tile<W>,
    /// Real position (view coordinates) and scale.
    from_loc: Point<f64, Logical>,
    from_scale: f64,
    slot: ExposeSlot,
}

impl<W: LayoutElement> Item<'_, W> {
    /// Position and scale at overview progress `p`.
    fn at(&self, p: f64) -> (Point<f64, Logical>, f64) {
        let loc = Point::from((
            lerp(self.from_loc.x, self.slot.rect.loc.x, p),
            lerp(self.from_loc.y, self.slot.rect.loc.y, p),
        ));
        (loc, lerp(self.from_scale, self.slot.scale, p).max(0.0001))
    }

    fn rect_at(&self, p: f64) -> Rect {
        let (loc, scale) = self.at(p);
        Rect::new(loc, self.tile.tile_size().upscale(scale))
    }
}

fn scale_key(scale: f64) -> u64 {
    scale.to_bits()
}

fn params() -> ExposeParams {
    ExposeParams {
        gap: GAP,
        label_height: LABEL_HEIGHT,
        max_scale: 1.,
    }
}

fn close_center(rect: Rect) -> Point<f64, Logical> {
    Point::from((
        rect.loc.x + rect.size.w - CLOSE_RADIUS - 4.,
        rect.loc.y + CLOSE_RADIUS + 4.,
    ))
}

fn dist2(a: Point<f64, Logical>, b: Point<f64, Logical>) -> f64 {
    let d = a - b;
    d.x * d.x + d.y * d.y
}

impl<W: LayoutElement> Monitor<W> {
    /// Whether the exposé is shown (open or animating).
    pub fn expose_active(&self) -> bool {
        self.overview_progress.is_some()
    }

    /// Overview progress, 0 (closed) to 1 (open); springs may overshoot slightly.
    fn expose_progress(&self) -> f64 {
        self.overview_progress
            .as_ref()
            .map_or(0., |p| p.value())
            .clamp(-0.1, 1.1)
    }

    fn expose_bar_strip(&self) -> Rect {
        let wa = self.working_area;
        let thumb_h = (self.view_size.h * 0.11).clamp(56., 120.);
        Rect::new(
            Point::from((wa.loc.x + SIDE_MARGIN, wa.loc.y + BAR_PADDING)),
            Size::from(((wa.size.w - SIDE_MARGIN * 2.).max(0.), thumb_h)),
        )
    }

    /// The area windows are laid out in: the working area below the bar.
    fn expose_area(&self) -> Rect {
        let wa = self.working_area;
        let bar = self.expose_bar_strip();
        let top = bar.loc.y + bar.size.h + BAR_PADDING + 8.;
        let bottom = wa.loc.y + wa.size.h - BOTTOM_MARGIN;
        Rect::new(
            Point::from((wa.loc.x + SIDE_MARGIN, top)),
            Size::from((
                (wa.size.w - SIDE_MARGIN * 2.).max(0.),
                (bottom - top).max(0.),
            )),
        )
    }

    /// Workspace thumbnails at progress `p` (they slide in from the top).
    fn expose_thumbs(&self, p: f64) -> Vec<Rect> {
        let strip = self.expose_bar_strip();
        let aspect = self.view_size.w / self.view_size.h.max(1.);
        let slide = -(strip.loc.y + strip.size.h + 8.) * (1. - p.clamp(0., 1.));
        expose::bar_layout(self.workspaces.len(), strip, aspect, BAR_GAP)
            .into_iter()
            .map(|mut r| {
                r.loc.y += slide;
                r
            })
            .collect()
    }

    /// The windows of `ws` with their real places and exposé slots, topmost first. Minimized
    /// windows and hidden tabs are left out.
    fn expose_items<'a>(&self, ws: &'a Workspace<W>) -> Vec<Item<'a, W>> {
        let tiles = ws.expose_tiles();
        let windows: Vec<Rect> = tiles
            .iter()
            .map(|(tile, pos, zoom)| {
                // Order by where the window is seen, size by its real size.
                let size = tile.tile_size();
                let seen = size.upscale(*zoom);
                let center = *pos + Point::from((seen.w / 2., seen.h / 2.));
                Rect::new(center - Point::from((size.w / 2., size.h / 2.)), size)
            })
            .collect();
        let ids: Vec<&W::Id> = tiles
            .iter()
            .map(|(tile, _, _)| tile.window().id())
            .collect();

        // Keep the slots from the last layout while the same windows are shown in the same
        // area.
        let area = self.expose_area();
        let mut cache = self.expose.slots.borrow_mut();
        let kept = cache
            .get(&ws.id())
            .filter(|(cached_area, cached)| *cached_area == area && cached.len() == ids.len())
            .and_then(|(_, cached)| {
                ids.iter()
                    .map(|id| cached.iter().find(|(c, _)| c == *id).map(|(_, slot)| *slot))
                    .collect::<Option<Vec<_>>>()
            });
        let slots = match kept {
            Some(slots) => slots,
            None => {
                let slots = expose::layout(&windows, area, &params());
                let entry = ids
                    .iter()
                    .map(|id| (*id).clone())
                    .zip(slots.iter().copied());
                cache.insert(ws.id(), (area, entry.collect()));
                slots
            }
        };
        drop(cache);

        tiles
            .into_iter()
            .zip(slots)
            .map(|((tile, pos, zoom), slot)| Item {
                tile,
                from_loc: pos,
                from_scale: zoom,
                slot,
            })
            .collect()
    }

    /// Workspaces drawn now (more than one during a switch), with their offsets.
    fn expose_workspaces(&self) -> impl Iterator<Item = (&Workspace<W>, Point<f64, Logical>)> {
        self.workspaces_with_render_geo()
            .map(|(ws, geo)| (ws, geo.loc))
    }

    pub(in crate::layout) fn expose_set_pointer(
        &mut self,
        pos: Option<Point<f64, Logical>>,
    ) -> bool {
        let pos = pos.filter(|_| self.expose_active());
        if self.expose.pointer == pos {
            return false;
        }
        self.expose.pointer = pos;
        self.expose_active()
    }

    /// The window under `pos` and whether `pos` is on its close button.
    fn expose_window_hit(&self, pos: Point<f64, Logical>) -> Option<(&W, bool)> {
        let p = self.expose_progress();
        for (ws, offset) in self.expose_workspaces() {
            for item in self.expose_items(ws) {
                let mut rect = item.rect_at(p);
                rect.loc += offset;
                let on_close =
                    p > 0.99 && dist2(pos, close_center(rect)) <= CLOSE_RADIUS * CLOSE_RADIUS;
                if on_close || rect.contains(pos) {
                    return Some((item.tile.window(), on_close));
                }
            }
        }
        None
    }

    fn expose_thumb_under(&self, pos: Point<f64, Logical>) -> Option<usize> {
        self.expose_thumbs(self.expose_progress())
            .iter()
            .position(|r| r.contains(pos))
    }

    pub(in crate::layout) fn expose_window_under(&self, pos: Point<f64, Logical>) -> Option<&W> {
        if self.expose_thumb_under(pos).is_some() {
            return None;
        }
        self.expose_window_hit(pos).map(|(win, _)| win)
    }

    pub(in crate::layout) fn expose_hit(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<ExposeHit<W::Id>> {
        if !self.expose_active() {
            return None;
        }
        if let Some(idx) = self.expose_thumb_under(pos) {
            return Some(ExposeHit::Workspace(idx));
        }
        if let Some((win, close)) = self.expose_window_hit(pos) {
            return Some(ExposeHit::Window {
                id: win.id().clone(),
                close,
            });
        }
        Some(ExposeHit::Empty)
    }

    pub(in crate::layout) fn expose_drag_begin(
        &mut self,
        window: &W::Id,
        pos: Point<f64, Logical>,
    ) {
        let p = self.expose_progress();
        let mut grab = Point::from((0.5, 0.5));
        for (ws, offset) in self.expose_workspaces() {
            for item in self.expose_items(ws) {
                if item.tile.window().id() == window {
                    let mut rect = item.rect_at(p);
                    rect.loc += offset;
                    grab = Point::from((
                        ((pos.x - rect.loc.x) / rect.size.w.max(1.)).clamp(0., 1.),
                        ((pos.y - rect.loc.y) / rect.size.h.max(1.)).clamp(0., 1.),
                    ));
                }
            }
        }
        self.expose.drag = Some(ExposeDrag {
            window: window.clone(),
            pos,
            grab,
        });
    }

    pub(in crate::layout) fn expose_drag_update(&mut self, pos: Point<f64, Logical>) {
        if let Some(drag) = &mut self.expose.drag {
            drag.pos = pos;
        }
        self.expose.pointer = Some(pos);
    }

    /// Ends a drag; returns the window and the workspace it was dropped on, if that is another
    /// workspace than the one it is on.
    pub(in crate::layout) fn expose_drag_end(&mut self) -> Option<(W::Id, usize)> {
        let drag = self.expose.drag.take()?;
        let idx = self.expose_thumb_under(drag.pos)?;
        let current = self
            .workspaces
            .iter()
            .position(|ws| ws.has_window(&drag.window))?;
        (idx != current).then_some((drag.window, idx))
    }

    // -------------------------------------------------------------------------------------
    // Rendering.

    /// Draws the exposé in place of the workspaces.
    pub(super) fn render_expose<R: ArborRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let _span = tracy_client::span!("Monitor::render_expose");

        let scale = self.scale.fractional_scale();
        let p = self.expose_progress();
        let alpha = p.clamp(0., 1.) as f32;
        let fade = (p.clamp(0., 1.) * p.clamp(0., 1.)) as f32;
        let settled = p > 0.99;

        {
            let drawn = std::mem::take(&mut *self.expose.drawn.lock().unwrap());
            let mut textures = self.expose.textures.borrow_mut();
            let mut in_flight = self.expose.in_flight.borrow_mut();
            for (key, buffer, elapsed) in drawn {
                crate::frame_stats::overview_drawing(elapsed);
                in_flight.remove(&key);
                textures.insert(
                    key,
                    CachedTex {
                        buffer,
                        texture: None,
                        used: false,
                    },
                );
            }
            for tex in textures.values_mut() {
                tex.used = false;
            }
        }

        let drag = self.expose.drag.as_ref();
        let pointer = self.expose.pointer;
        let thumbs = self.expose_thumbs(p);
        let drop_target = drag.and_then(|d| thumbs.iter().position(|r| r.contains(d.pos)));
        let hover_thumb = pointer.and_then(|pos| thumbs.iter().position(|r| r.contains(pos)));

        // The dragged window, on top of everything.
        let hovered = if drag.is_some() || hover_thumb.is_some() {
            None
        } else {
            pointer.and_then(|pos| self.expose_window_hit(pos))
        };

        let mut tile_elems: Vec<(Point<f64, Logical>, f64, &Tile<W>)> = Vec::new();
        let mut decorations: Vec<(TexKey, Point<f64, Logical>, f32)> = Vec::new();
        let mut outlines: Vec<(Rect, bool, f32)> = Vec::new();

        let active_ws = self.active_workspace_idx;
        let mut dragged: Option<(Point<f64, Logical>, f64, &Tile<W>)> = None;

        for (ws, offset) in self.expose_workspaces() {
            let focused = ws.active_window().map(|w| w.id().clone());
            for item in self.expose_items(ws) {
                let id = item.tile.window().id();
                let (mut loc, item_scale) = item.at(p);
                loc += offset;
                let size = item.tile.tile_size().upscale(item_scale);
                let rect = Rect::new(loc, size);

                if let Some(d) = drag.filter(|d| &d.window == id) {
                    // Over the bar, shrink to fit a thumbnail.
                    let mut s = item.slot.scale;
                    if let Some(t) = drop_target.and_then(|idx| thumbs.get(idx)) {
                        let tile_size = item.tile.tile_size();
                        s = s
                            .min(t.size.w * 0.8 / tile_size.w.max(1.))
                            .min(t.size.h * 0.8 / tile_size.h.max(1.));
                    }
                    let size = item.tile.tile_size().upscale(s);
                    let loc = d.pos - Point::from((d.grab.x * size.w, d.grab.y * size.h));
                    dragged = Some((loc, s, item.tile));
                    continue;
                }

                // Title and icon under the window.
                let title = item.tile.window().title().unwrap_or_default();
                let app_id = item.tile.window().app_id().unwrap_or_default();
                let label_w = item.slot.rect.size.w.max(LABEL_ICON * 4.);
                let key = TexKey::Label {
                    has_icon: !app_id.is_empty() && app_icon(&app_id).is_some(),
                    title,
                    app_id,
                    width: to_physical_precise_round::<i32>(scale, label_w),
                    scale: scale_key(scale),
                };
                let label_loc = Point::from((
                    rect.loc.x + (rect.size.w - label_w) / 2.,
                    rect.loc.y + rect.size.h + 6.,
                ));
                decorations.push((key, label_loc, fade));

                // Outline: accent on hover, subtle on the focused window (keyboard).
                let is_hovered = hovered.as_ref().is_some_and(|(w, _)| w.id() == id);
                let is_focused =
                    ws.id() == self.workspaces[active_ws].id() && focused.as_ref() == Some(id);
                if is_hovered || is_focused {
                    let outer = Rect::new(
                        rect.loc - Point::from((OUTLINE_PAD, OUTLINE_PAD)),
                        Size::from((size.w + OUTLINE_PAD * 2., size.h + OUTLINE_PAD * 2.)),
                    );
                    outlines.push((outer, is_hovered, alpha));
                }
                if is_hovered && settled {
                    let c = close_center(rect);
                    let key = TexKey::Close {
                        scale: scale_key(scale),
                    };
                    let loc = c - Point::from((CLOSE_RADIUS + 2., CLOSE_RADIUS + 2.));
                    decorations.push((key, loc, 1.));
                }

                tile_elems.push((loc, item_scale, item.tile));
            }
        }

        // Draw order: first pushed is on top.
        if let Some((loc, s, tile)) = dragged {
            self.push_expose_tile(ctx.r(), tile, loc, s, push);
        }

        // Close buttons, outlines and labels go above the windows.
        let is_close = |k: &TexKey| matches!(k, TexKey::Close { .. });
        for (key, loc, a) in decorations.iter().filter(|(k, _, _)| is_close(k)) {
            if let Some(elem) = self.expose_texture(ctx.renderer, key.clone(), *loc, *a) {
                push(tex_elem(elem));
            }
        }
        if BorderRenderElement::has_shader(ctx.renderer) {
            self.push_outlines(&outlines, push);
        }
        for (key, loc, a) in decorations.iter().filter(|(k, _, _)| !is_close(k)) {
            if let Some(elem) = self.expose_texture(ctx.renderer, key.clone(), *loc, *a) {
                push(tex_elem(elem));
            }
        }

        // The workspace bar: per thumbnail, its front layer (icons, name, border), the
        // workspace's real windows scaled into it, then its back layer.
        let view = self.view_size;
        for (idx, (rect, ws)) in thumbs.iter().zip(&self.workspaces).enumerate() {
            let front = self.thumb_key(
                ws,
                *rect,
                scale,
                idx == active_ws,
                hover_thumb == Some(idx) || drop_target == Some(idx),
            );
            let back = ThumbKey {
                back: true,
                is_hovered: false,
                is_active: false,
                name: String::new(),
                windows: Vec::new(),
                ..front.clone()
            };
            self.push_thumb_layer(ctx.renderer, idx, false, front, rect.loc, alpha, push);
            if alpha > 0. {
                let k = rect.size.w / view.w.max(1.);
                // Topmost first, as the elements are pushed.
                for (tile, pos, zoom) in ws.expose_tiles() {
                    if drag.is_some_and(|d| &d.window == tile.window().id()) {
                        continue;
                    }
                    let loc = rect.loc + Point::from((pos.x * k, pos.y * k));
                    self.push_thumb_tile(ctx.r(), tile, loc, zoom * k, *rect, push);
                }
            }
            self.push_thumb_layer(ctx.renderer, idx, true, back, rect.loc, alpha, push);
        }

        // The windows.
        for (loc, s, tile) in tile_elems {
            self.push_expose_tile(ctx.r(), tile, loc, s, push);
        }

        // The dimmed wallpaper (the background layers draw below this).
        let mut dim = self.expose.dim.borrow_mut();
        dim.resize(self.view_size);
        let elem = SolidColorRenderElement::from_buffer(
            &dim,
            (0., 0.),
            DIM_ALPHA * alpha,
            Kind::Unspecified,
        );
        let elem = MonitorInnerRenderElement::SolidColor(elem);
        let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), 1.);
        push(RelocateRenderElement::from_element(
            elem,
            Point::from((0, 0)),
            Relocate::Relative,
        ));

        self.expose.textures.borrow_mut().retain(|_, tex| tex.used);
        // Forget slots of workspaces that are gone (they hold window ids).
        self.expose
            .slots
            .borrow_mut()
            .retain(|id, _| self.workspaces.iter().any(|ws| ws.id() == *id));
    }

    /// One layer of a workspace thumbnail. Until a new look (hovered, a window moved...) is
    /// drawn, the previous one stands in, instead of the thumbnail blinking out for a frame.
    #[allow(clippy::too_many_arguments)]
    fn push_thumb_layer<R: ArborRenderer>(
        &self,
        renderer: &mut R,
        idx: usize,
        back: bool,
        key: ThumbKey,
        loc: Point<f64, Logical>,
        alpha: f32,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let key = TexKey::Thumb(key);
        let ready = self.expose_texture(renderer, key.clone(), loc, alpha);
        let elem = match ready {
            Some(elem) => {
                self.expose.shown_thumbs.borrow_mut().insert((idx, back), key);
                Some(elem)
            }
            None => {
                let old = self
                    .expose
                    .shown_thumbs
                    .borrow()
                    .get(&(idx, back))
                    .filter(|old| self.expose.textures.borrow().contains_key(*old))
                    .cloned();
                old.and_then(|old| self.expose_texture(renderer, old, loc, alpha))
            }
        };
        if let Some(elem) = elem {
            push(tex_elem(elem));
        }
    }

    /// Rounded outlines drawn by the border shader: no CPU rasterizing, whatever their size.
    fn push_outlines<R: ArborRenderer>(
        &self,
        outlines: &[(Rect, bool, f32)],
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let scale = self.scale.fractional_scale();
        let mut pool = self.expose.outlines.borrow_mut();
        if pool.len() < outlines.len() {
            pool.resize_with(outlines.len(), BorderRenderElement::empty);
        }
        for (border, (rect, hovered, alpha)) in pool.iter_mut().zip(outlines) {
            let loc = rect.loc.to_physical_precise_round(scale).to_logical(scale);
            let size = rect.size.to_physical_precise_round(scale).to_logical(scale);
            let area = Rectangle::from_size(size);
            let (width, color) = if *hovered {
                let (r, g, b) = ACCENT;
                (
                    OUTLINE_WIDTH,
                    Color::new_unpremul(r as f32, g as f32, b as f32, 1.),
                )
            } else {
                (2., Color::new_unpremul(1., 1., 1., 0.5))
            };
            border.update(
                size,
                area,
                GradientInterpolation::default(),
                color,
                color,
                0.,
                area,
                width as f32,
                CornerRadius::from(12.),
                scale as f32,
                *alpha,
            );
            let elem = MonitorInnerRenderElement::ExposeOutline(border.clone().with_location(loc));
            let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), 1.);
            push(RelocateRenderElement::from_element(
                elem,
                Point::from((0, 0)),
                Relocate::Relative,
            ));
        }
    }

    fn push_expose_tile<R: ArborRenderer>(
        &self,
        ctx: RenderCtx<R>,
        tile: &Tile<W>,
        loc: Point<f64, Logical>,
        tile_scale: f64,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let scale = self.scale.fractional_scale();
        let loc = loc.to_physical_precise_round(scale).to_logical(scale);
        let xray_pos = XrayPos::new(loc, tile_scale);
        tile.render(ctx, Point::from((0., 0.)), xray_pos, false, &mut |elem| {
            let elem = MonitorInnerRenderElement::ExposeTile(elem);
            let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), tile_scale);
            push(RelocateRenderElement::from_element(
                elem,
                loc.to_physical_precise_round(scale),
                Relocate::Relative,
            ));
        });
    }

    /// A window in a workspace thumbnail: live, scaled down and cut to the thumbnail (windows
    /// reaching past the screen edge would spill out of it).
    fn push_thumb_tile<R: ArborRenderer>(
        &self,
        ctx: RenderCtx<R>,
        tile: &Tile<W>,
        loc: Point<f64, Logical>,
        tile_scale: f64,
        clip: Rect,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let scale = self.scale.fractional_scale();
        let output_scale = smithay::utils::Scale::from(scale);
        let loc = loc.to_physical_precise_round(scale).to_logical(scale);
        let clip = clip.to_physical_precise_round(scale);
        let xray_pos = XrayPos::new(loc, tile_scale);
        tile.render(ctx, Point::from((0., 0.)), xray_pos, false, &mut |elem| {
            let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), tile_scale);
            let elem = RelocateRenderElement::from_element(
                elem,
                loc.to_physical_precise_round(scale),
                Relocate::Relative,
            );
            let Some(elem) = CropRenderElement::from_element(elem, output_scale, clip) else {
                return;
            };
            let elem = MonitorInnerRenderElement::ExposeThumbTile(elem);
            let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), 1.);
            push(RelocateRenderElement::from_element(
                elem,
                Point::from((0, 0)),
                Relocate::Relative,
            ));
        });
    }

    fn thumb_key(
        &self,
        ws: &Workspace<W>,
        rect: Rect,
        scale: f64,
        is_active: bool,
        is_hovered: bool,
    ) -> ThumbKey {
        let view = self.view_size;
        let windows: Vec<_> = ws
            .expose_tiles()
            .into_iter()
            .rev() // Bottom first, so the topmost window paints last.
            .map(|(tile, pos, zoom)| {
                let size = tile.tile_size().upscale(zoom);
                let rel = |v: f64, total: f64| (v / total.max(1.) * 10000.).round() as i32;
                let app_id = tile.window().app_id().unwrap_or_default();
                let has_icon = !app_id.is_empty() && app_icon(&app_id).is_some();
                (
                    (
                        rel(pos.x, view.w),
                        rel(pos.y, view.h),
                        rel(size.w, view.w),
                        rel(size.h, view.h),
                    ),
                    app_id,
                    has_icon,
                )
            })
            .collect();
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id() == ws.id())
            .unwrap_or(0);
        ThumbKey {
            back: false,
            size: (
                to_physical_precise_round::<i32>(scale, rect.size.w),
                to_physical_precise_round::<i32>(scale, rect.size.h),
            ),
            scale: scale_key(scale),
            name: ws.name().cloned().unwrap_or_else(|| format!("{}", idx + 1)),
            is_active,
            is_hovered,
            windows,
        }
    }

    fn expose_texture<R: ArborRenderer>(
        &self,
        renderer: &mut R,
        key: TexKey,
        loc: Point<f64, Logical>,
        alpha: f32,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        if alpha <= 0. {
            return None;
        }
        let scale = self.scale.fractional_scale();
        let mut textures = self.expose.textures.borrow_mut();
        let Some(tex) = textures.get_mut(&key) else {
            // Rasterize on a background thread so a frame never waits for cairo and pango (a
            // dozen labels and thumbnails at 5K take tens of ms); it fades in a frame later.
            if self.expose.in_flight.borrow_mut().insert(key.clone()) {
                let drawn = self.expose.drawn.clone();
                let res = std::thread::Builder::new()
                    .name("arbor-overview-draw".to_owned())
                    .spawn(move || {
                        let start = std::time::Instant::now();
                        let buffer = match draw(&key, scale) {
                            Ok(buffer) => buffer,
                            Err(err) => {
                                warn!("error drawing overview element: {err:?}");
                                None
                            }
                        };
                        let elapsed = start.elapsed();
                        drawn.lock().unwrap().push((key, buffer, elapsed));
                        crate::layout::icon_tile::request_redraw();
                    });
                if let Err(err) = res {
                    warn!("error spawning overview drawing thread: {err:?}");
                }
            }
            return None;
        };
        tex.used = true;
        let buffer = tex.buffer.as_ref()?;
        if tex.texture.is_none() {
            tex.texture =
                TextureBuffer::from_memory_buffer(renderer.as_gles_renderer(), buffer).ok();
        }
        let texture = tex.texture.clone()?;
        let loc = loc.to_physical_precise_round(scale).to_logical(scale);
        let elem = TextureRenderElement::from_texture_buffer(
            texture,
            loc,
            alpha,
            None,
            None,
            Kind::Unspecified,
        );
        Some(PrimaryGpuTextureRenderElement(elem))
    }
}

/// A texture placed in logical output coordinates.
fn tex_elem<R: ArborRenderer>(elem: PrimaryGpuTextureRenderElement) -> MonitorRenderElement<R> {
    let elem = MonitorInnerRenderElement::ExposeTexture(elem);
    let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), 1.);
    RelocateRenderElement::from_element(elem, Point::from((0, 0)), Relocate::Relative)
}

// -----------------------------------------------------------------------------------------
// Drawing (cairo).

fn surface(w: i32, h: i32) -> anyhow::Result<(ImageSurface, cairo::Context)> {
    let surface = ImageSurface::create(cairo::Format::ARgb32, w.max(1), h.max(1))?;
    let cr = cairo::Context::new(&surface)?;
    Ok((surface, cr))
}

fn finish(surface: ImageSurface, cr: cairo::Context, scale: f64) -> anyhow::Result<MemoryBuffer> {
    let (w, h) = (surface.width(), surface.height());
    drop(cr);
    let data = surface.take_data()?;
    Ok(MemoryBuffer::new(
        data.to_vec(),
        Fourcc::Argb8888,
        (w, h),
        scale,
        Transform::Normal,
    ))
}

fn draw(key: &TexKey, scale: f64) -> anyhow::Result<Option<MemoryBuffer>> {
    let _span = tracy_client::span!("monitor_expose::draw");
    let s = scale;
    Ok(Some(match key {
        TexKey::Label {
            title,
            app_id,
            width,
            ..
        } => {
            let h = to_physical_precise_round::<i32>(s, LABEL_HEIGHT - 6.);
            if *width <= 0 || h <= 0 {
                return Ok(None);
            }
            let (surf, cr) = surface(*width, h)?;
            let (wf, hf) = (f64::from(*width), f64::from(h));
            let icon_side = LABEL_ICON * s;
            let spacing = 6. * s;

            let mut font = FontDescription::from_string("sans 10");
            font.set_absolute_size(to_physical_precise_round(s, font.size()));
            let layout = pangocairo::functions::create_layout(&cr);
            layout.context().set_round_glyph_positions(false);
            layout.set_font_description(Some(&font));
            layout.set_ellipsize(EllipsizeMode::End);
            layout.set_single_paragraph_mode(true);
            let text = if title.is_empty() { app_id } else { title };
            layout.set_text(text);
            let max_text = (wf - icon_side - spacing - 16. * s).max(0.);
            layout.set_width((max_text * f64::from(pangocairo::pango::SCALE)) as i32);
            let (tw, th) = layout.pixel_size();
            let tw = f64::from(tw).min(max_text);
            let content_w = icon_side + spacing + tw;
            let x0 = ((wf - content_w) / 2.).max(0.);

            // A soft pill behind, readable on any wallpaper.
            let pill_h = (f64::from(th) + 8. * s).min(hf);
            let pill_y = (hf - pill_h) / 2.;
            rounded_rect(
                &cr,
                x0 - 8. * s,
                pill_y,
                content_w + 16. * s,
                pill_h,
                pill_h / 2.,
            );
            cr.set_source_rgba(0., 0., 0., 0.45);
            cr.fill()?;

            let icon_y = (hf - icon_side) / 2.;
            match (!app_id.is_empty()).then(|| app_icon(app_id)).flatten() {
                Some(icon) => paint_icon(&cr, &icon, x0, icon_y, icon_side)?,
                None => {
                    let (r, g, b) = app_color(app_id);
                    rounded_rect(&cr, x0, icon_y, icon_side, icon_side, 4. * s);
                    cr.set_source_rgb(r, g, b);
                    cr.fill()?;
                    let mut f = FontDescription::from_string("sans bold");
                    f.set_absolute_size(icon_side * 0.6 * f64::from(pangocairo::pango::SCALE));
                    let l = pangocairo::functions::create_layout(&cr);
                    l.set_font_description(Some(&f));
                    l.set_text(&app_letter(app_id));
                    let (lw, lh) = l.pixel_size();
                    cr.move_to(
                        x0 + (icon_side - f64::from(lw)) / 2.,
                        icon_y + (icon_side - f64::from(lh)) / 2.,
                    );
                    cr.set_source_rgba(1., 1., 1., 0.95);
                    pangocairo::functions::show_layout(&cr, &l);
                }
            }

            cr.move_to(x0 + icon_side + spacing, (hf - f64::from(th)) / 2.);
            cr.set_source_rgba(1., 1., 1., 0.95);
            pangocairo::functions::show_layout(&cr, &layout);
            finish(surf, cr, s)?
        }
        TexKey::Close { .. } => {
            let side = to_physical_precise_round::<i32>(s, (CLOSE_RADIUS + 2.) * 2.);
            let (surf, cr) = surface(side, side)?;
            let c = f64::from(side) / 2.;
            let r = CLOSE_RADIUS * s;
            cr.arc(c, c, r, 0., 2. * PI);
            cr.set_source_rgba(0.12, 0.12, 0.14, 0.92);
            cr.fill_preserve()?;
            cr.set_source_rgba(1., 1., 1., 0.35);
            cr.set_line_width(s.max(1.));
            cr.stroke()?;
            let a = r * 0.38;
            cr.move_to(c - a, c - a);
            cr.line_to(c + a, c + a);
            cr.move_to(c + a, c - a);
            cr.line_to(c - a, c + a);
            cr.set_source_rgba(1., 1., 1., 0.95);
            cr.set_line_width((1.8 * s).max(1.));
            cr.set_line_cap(cairo::LineCap::Round);
            cr.stroke()?;
            finish(surf, cr, s)?
        }
        TexKey::Thumb(key) => {
            let (w, h) = key.size;
            if w <= 0 || h <= 0 {
                return Ok(None);
            }
            let (surf, cr) = surface(w, h)?;
            let (wf, hf) = (f64::from(w), f64::from(h));
            let radius = 10. * s;

            if key.back {
                // Under the live windows: the background. The spare empty workspace at the end
                // looks like any blank desktop (no "+": there is always a fresh one).
                rounded_rect(&cr, 0., 0., wf, hf, radius);
                cr.set_source_rgba(0.1, 0.11, 0.13, 0.82);
                cr.fill()?;
                return Ok(Some(finish(surf, cr, s)?));
            }

            // Over the live windows: each app's icon in front of its window, like macOS.
            rounded_rect(&cr, 0., 0., wf, hf, radius);
            cr.save()?;
            cr.clip();
            for ((x, y, ww, hh), app_id, _) in &key.windows {
                let x = f64::from(*x) / 10000. * wf;
                let y = f64::from(*y) / 10000. * hf;
                let ww = f64::from(*ww) / 10000. * wf;
                let hh = f64::from(*hh) / 10000. * hf;
                if ww < 1. || hh < 1. {
                    continue;
                }
                let side = (ww.min(hh) * 0.55).min(28. * s);
                if side < 6. {
                    continue;
                }
                let (ix, iy) = (x + (ww - side) / 2., y + (hh - side) / 2.);
                match (!app_id.is_empty()).then(|| app_icon(app_id)).flatten() {
                    Some(icon) => paint_icon(&cr, &icon, ix, iy, side)?,
                    None => {
                        let (r, g, b) = app_color(app_id);
                        rounded_rect(&cr, ix, iy, side, side, side * 0.22);
                        cr.set_source_rgb(r, g, b);
                        cr.fill()?;
                    }
                }
            }
            cr.restore()?;

            // Name in the corner.
            let mut font = FontDescription::from_string("sans 9");
            font.set_weight(Weight::Bold);
            font.set_absolute_size(to_physical_precise_round(s, font.size()));
            let layout = pangocairo::functions::create_layout(&cr);
            layout.set_font_description(Some(&font));
            layout.set_alignment(Alignment::Left);
            layout.set_ellipsize(EllipsizeMode::End);
            layout.set_width(((wf - 12. * s).max(0.) * f64::from(pangocairo::pango::SCALE)) as i32);
            layout.set_text(&key.name);
            let (tw, th) = layout.pixel_size();
            rounded_rect(
                &cr,
                4. * s,
                4. * s,
                f64::from(tw) + 8. * s,
                f64::from(th) + 2. * s,
                4. * s,
            );
            cr.set_source_rgba(0., 0., 0., 0.5);
            cr.fill()?;
            cr.move_to(8. * s, 5. * s);
            cr.set_source_rgba(1., 1., 1., 0.95);
            pangocairo::functions::show_layout(&cr, &layout);

            // Border: accent for the shown workspace, white for hover / drop target.
            let (r, g, b) = ACCENT;
            let lw = if key.is_active || key.is_hovered {
                2.5 * s
            } else {
                s.max(1.)
            };
            rounded_rect(&cr, lw / 2., lw / 2., wf - lw, hf - lw, radius);
            if key.is_active {
                cr.set_source_rgba(r, g, b, 1.);
            } else if key.is_hovered {
                cr.set_source_rgba(1., 1., 1., 0.8);
            } else {
                cr.set_source_rgba(1., 1., 1., 0.18);
            }
            cr.set_line_width(lw);
            cr.stroke()?;
            if key.is_hovered && key.is_active {
                rounded_rect(&cr, lw * 1.5, lw * 1.5, wf - lw * 3., hf - lw * 3., radius);
                cr.set_source_rgba(1., 1., 1., 0.6);
                cr.set_line_width(s.max(1.));
                cr.stroke()?;
            }
            finish(surf, cr, s)?
        }
    }))
}
