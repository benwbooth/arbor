//! Permission and source-picker dialogs for arbor's portal backend.
//!
//! A portal request (share the screen, control the keyboard and mouse, ...) shows one dialog at
//! a time, centred on the active output over a dimmed backdrop: a title, a plain-language
//! explanation, one button per choice and, when the app asked for it, a "remember" checkbox.
//! Click a choice or press its number (or move with the arrow keys and press Enter); Escape or
//! a click outside cancels. Further requests wait in a queue.

use std::cell::RefCell;
use std::collections::VecDeque;

use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::{FontDescription, Weight};
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::Kind;
use smithay::output::Output;
use smithay::reexports::gbm::Format as Fourcc;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use crate::render_helpers::memory::MemoryBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::ArborRenderer;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::ui::exit_confirm_dialog::ExitConfirmDialogRenderElement;
use crate::utils::{output_size, to_physical_precise_round};

const PADDING: f64 = 20.;
const ROW_HEIGHT: f64 = 40.;
const ROW_GAP: f64 = 8.;
const MIN_WIDTH: f64 = 460.;
const MAX_TEXT_WIDTH: f64 = 640.;
const TITLE_FONT: &str = "sans 17px";
const FONT: &str = "sans 14px";
const BACKDROP_COLOR: [f32; 4] = [0., 0., 0., 0.45];

/// What the user answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAnswer {
    /// Index into [`DialogRequest::choices`], and the state of the "remember" checkbox.
    Chosen {
        index: usize,
        remember: bool,
    },
    Cancelled,
}

pub struct DialogRequest {
    pub title: String,
    pub body: String,
    pub choices: Vec<String>,
    /// Label of the "remember" checkbox and its initial state, if it is shown.
    pub remember: Option<(String, bool)>,
    pub reply: async_channel::Sender<DialogAnswer>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Choice(usize),
    Remember,
    Cancel,
}

struct Open {
    request: DialogRequest,
    selected: usize,
    remember: bool,
    /// Rendered texture for (scale) and hit rectangles in logical, dialog-local coordinates.
    cache: RefCell<Option<(f64, MemoryBuffer, Hits)>>,
    /// Where the dialog was last drawn (global logical rectangle).
    drawn_at: RefCell<Option<Rectangle<f64, Logical>>>,
}

#[derive(Default)]
pub struct PortalDialog {
    open: Option<Open>,
    queue: VecDeque<DialogRequest>,
    backdrop: RefCell<Option<SolidColorBuffer>>,
}

impl PortalDialog {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Queues a dialog. Returns true if it is shown right away.
    pub fn show(&mut self, request: DialogRequest) -> bool {
        if self.open.is_some() {
            self.queue.push_back(request);
            return false;
        }
        self.open_request(request);
        true
    }

    fn open_request(&mut self, request: DialogRequest) {
        let remember = request.remember.as_ref().is_some_and(|(_, on)| *on);
        self.open = Some(Open {
            request,
            selected: 0,
            remember,
            cache: RefCell::new(None),
            drawn_at: RefCell::new(None),
        });
    }

    fn finish(&mut self, answer: DialogAnswer) {
        if let Some(open) = self.open.take() {
            let _ = open.request.reply.try_send(answer);
        }
        // Skip requests whose requester has given up meanwhile.
        while let Some(next) = self.queue.pop_front() {
            if !next.reply.is_closed() {
                self.open_request(next);
                break;
            }
        }
    }

    /// Drops the open dialog (and queued ones) whose requester went away.
    pub fn prune(&mut self) -> bool {
        let before = self.queue.len();
        self.queue.retain(|r| !r.reply.is_closed());
        let mut changed = before != self.queue.len();
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.request.reply.is_closed())
        {
            self.finish(DialogAnswer::Cancelled);
            changed = true;
        }
        changed
    }

    pub fn cancel(&mut self) {
        self.finish(DialogAnswer::Cancelled);
    }

    fn invalidate(&self) {
        if let Some(open) = &self.open {
            open.cache.borrow_mut().take();
        }
    }

    /// Handles a key press (by keysym). Returns true if the dialog changed (needs a redraw).
    pub fn key(&mut self, keysym: smithay::input::keyboard::Keysym) -> bool {
        use smithay::input::keyboard::Keysym as K;
        let Some(open) = &mut self.open else {
            return false;
        };
        let n = open.request.choices.len();
        match keysym {
            K::Escape => self.finish(DialogAnswer::Cancelled),
            K::Return | K::KP_Enter | K::space => {
                let answer = DialogAnswer::Chosen {
                    index: open.selected,
                    remember: open.remember,
                };
                self.finish(answer);
            }
            K::Down | K::Tab | K::Right => {
                open.selected = (open.selected + 1) % n.max(1);
                self.invalidate();
            }
            K::Up | K::ISO_Left_Tab | K::Left => {
                open.selected = (open.selected + n.max(1) - 1) % n.max(1);
                self.invalidate();
            }
            K::r | K::R if open.request.remember.is_some() => {
                open.remember = !open.remember;
                self.invalidate();
            }
            _ => {
                // Number keys pick directly.
                let digit = match keysym.raw() {
                    k @ 0x31..=0x39 => Some((k - 0x31) as usize),
                    k @ 0xffb1..=0xffb9 => Some((k - 0xffb1) as usize),
                    _ => None,
                };
                let Some(index) = digit.filter(|&i| i < n) else {
                    return false;
                };
                let remember = open.remember;
                self.finish(DialogAnswer::Chosen { index, remember });
            }
        }
        true
    }

    /// Handles a left click at a global logical position. Returns true if consumed.
    pub fn click(&mut self, pos: Point<f64, Logical>) -> bool {
        let Some(open) = &mut self.open else {
            return false;
        };
        let Some(rect) = *open.drawn_at.borrow() else {
            return true;
        };
        if !rect.contains(pos) {
            // Clicks outside do nothing (the dialog is modal); Escape or Cancel closes it.
            return true;
        }
        let local = pos - rect.loc;
        let hit = open.cache.borrow().as_ref().and_then(|(_, _, hits)| {
            hits.iter()
                .find(|(_, r)| r.contains(local))
                .map(|(hit, _)| *hit)
        });
        match hit {
            Some(Hit::Choice(index)) => {
                let remember = open.remember;
                self.finish(DialogAnswer::Chosen { index, remember });
            }
            Some(Hit::Remember) => {
                open.remember = !open.remember;
                self.invalidate();
            }
            Some(Hit::Cancel) => self.finish(DialogAnswer::Cancelled),
            None => (),
        }
        true
    }

    pub fn render<R: ArborRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
        output_loc: Point<i32, Logical>,
        push: &mut dyn FnMut(ExitConfirmDialogRenderElement),
    ) {
        let Some(open) = &self.open else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let output_size = output_size(output);

        let mut cache = open.cache.borrow_mut();
        if cache.as_ref().is_none_or(|(s, _, _)| *s != scale) {
            match render(&open.request, open.selected, open.remember, scale) {
                Ok((buffer, hits)) => *cache = Some((scale, buffer, hits)),
                Err(err) => {
                    warn!("error rendering the portal dialog: {err:?}");
                    return;
                }
            }
        }
        let (_, buffer, _) = cache.as_ref().unwrap();

        let size = buffer.logical_size();
        let Ok(texture) = TextureBuffer::from_memory_buffer(renderer.as_gles_renderer(), buffer)
        else {
            return;
        };
        let location = (output_size.to_point() - size.to_point()).downscale(2.);
        let mut location = location.to_physical_precise_round(scale).to_logical(scale);
        location.x = f64::max(0., location.x);
        location.y = f64::max(0., location.y);
        *open.drawn_at.borrow_mut() = Some(Rectangle::new(location + output_loc.to_f64(), size));

        let elem = TextureRenderElement::from_texture_buffer(
            texture,
            location,
            1.,
            None,
            None,
            Kind::Unspecified,
        );
        let elem = PrimaryGpuTextureRenderElement(elem);
        let elem =
            RescaleRenderElement::from_element(elem, location.to_physical_precise_round(scale), 1.);
        push(ExitConfirmDialogRenderElement::Texture(elem));

        let mut backdrop = self.backdrop.borrow_mut();
        let backdrop =
            backdrop.get_or_insert_with(|| SolidColorBuffer::new(output_size, BACKDROP_COLOR));
        backdrop.resize(output_size);
        let elem = SolidColorRenderElement::from_buffer(
            backdrop,
            Point::new(0., 0.),
            1.,
            Kind::Unspecified,
        );
        push(ExitConfirmDialogRenderElement::SolidColor(elem));
    }
}

fn escape(text: &str) -> String {
    pangocairo::glib::markup_escape_text(text).to_string()
}

type Hits = Vec<(Hit, Rectangle<f64, Logical>)>;

fn render(
    request: &DialogRequest,
    selected: usize,
    remember: bool,
    scale: f64,
) -> anyhow::Result<(MemoryBuffer, Hits)> {
    let _span = tracy_client::span!("portal_dialog::render");

    let px = |v: f64| -> f64 { (v * scale).round() };

    let mut title_font = FontDescription::from_string(TITLE_FONT);
    title_font.set_weight(Weight::Bold);
    title_font.set_absolute_size(to_physical_precise_round(scale, title_font.size()));
    let mut font = FontDescription::from_string(FONT);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    let measure = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&measure)?;
    let make_layout = |cr: &cairo::Context, font: &FontDescription, markup: &str, wrap: bool| {
        let layout = pangocairo::functions::create_layout(cr);
        layout.context().set_round_glyph_positions(false);
        layout.set_font_description(Some(font));
        if wrap {
            layout.set_width((px(MAX_TEXT_WIDTH) * f64::from(pangocairo::pango::SCALE)) as i32);
            layout.set_wrap(pangocairo::pango::WrapMode::WordChar);
        }
        layout.set_markup(markup);
        layout
    };

    let choice_text = |i: usize, label: &str| format!("<b>{}</b>   {}", i + 1, escape(label));
    let remember_text = |label: &str| {
        format!(
            "{}  {}   <span alpha='60%'>(R)</span>",
            if remember { "☑" } else { "☐" },
            escape(label)
        )
    };
    let footer = "<span alpha='70%'>Click a choice or press its number · Enter chooses the \
                  highlighted one · Esc cancels</span>";

    let title_l = make_layout(&cr, &title_font, &escape(&request.title), true);
    let body_l = make_layout(&cr, &font, &escape(&request.body), true);
    let footer_l = make_layout(&cr, &font, footer, true);
    let mut width = px(MIN_WIDTH) - 2. * px(PADDING);
    for l in [&title_l, &body_l, &footer_l] {
        width = width.max(f64::from(l.pixel_size().0));
    }
    for (i, label) in request.choices.iter().enumerate() {
        let l = make_layout(&cr, &font, &choice_text(i, label), false);
        width = width.max(f64::from(l.pixel_size().0) + 2. * px(16.));
    }
    if let Some((label, _)) = &request.remember {
        let l = make_layout(&cr, &font, &remember_text(label), false);
        width = width.max(f64::from(l.pixel_size().0));
    }
    drop(cr);

    let pad = px(PADDING);
    let row_h = px(ROW_HEIGHT);
    let gap = px(ROW_GAP);
    let title_h = f64::from(title_l.pixel_size().1);
    let body_h = f64::from(body_l.pixel_size().1);
    let footer_h = f64::from(footer_l.pixel_size().1);
    let n = request.choices.len() as f64;
    let remember_h = if request.remember.is_some() {
        row_h
    } else {
        0.
    };

    let total_w = (width + 2. * pad).ceil();
    let mut total_h = pad + title_h + gap + body_h + 2. * gap;
    total_h += n * row_h + (n - 1.).max(0.) * gap;
    total_h += gap + remember_h + gap + row_h + gap + footer_h + pad;
    let total_h = total_h.ceil();

    let surface = ImageSurface::create(cairo::Format::ARgb32, total_w as i32, total_h as i32)?;
    let cr = cairo::Context::new(&surface)?;
    rounded_rect(&cr, 0., 0., total_w, total_h, px(14.));
    cr.set_source_rgb(0.13, 0.13, 0.14);
    cr.fill_preserve()?;
    cr.set_source_rgba(1., 1., 1., 0.12);
    cr.set_line_width(px(1.).max(1.));
    cr.stroke()?;

    let mut hits = Vec::new();
    let to_logical = |x: f64, y: f64, w: f64, h: f64| {
        Rectangle::new(
            Point::from((x / scale, y / scale)),
            Size::from((w / scale, h / scale)),
        )
    };

    let mut y = pad;
    cr.set_source_rgb(1., 1., 1.);
    cr.move_to(pad, y);
    let l = make_layout(&cr, &title_font, &escape(&request.title), true);
    pangocairo::functions::show_layout(&cr, &l);
    y += title_h + gap;

    cr.set_source_rgba(1., 1., 1., 0.85);
    cr.move_to(pad, y);
    let l = make_layout(&cr, &font, &escape(&request.body), true);
    pangocairo::functions::show_layout(&cr, &l);
    y += body_h + 2. * gap;

    let button = |cr: &cairo::Context,
                  y: f64,
                  markup: &str,
                  highlighted: bool,
                  accent: bool|
     -> anyhow::Result<()> {
        rounded_rect(cr, pad, y, width, row_h, px(8.));
        if accent && highlighted {
            cr.set_source_rgb(0.21, 0.47, 0.93);
        } else if highlighted {
            cr.set_source_rgba(1., 1., 1., 0.18);
        } else {
            cr.set_source_rgba(1., 1., 1., 0.07);
        }
        cr.fill()?;
        let l = make_layout(cr, &font, markup, false);
        let th = f64::from(l.pixel_size().1);
        cr.set_source_rgb(1., 1., 1.);
        cr.move_to(pad + px(16.), y + (row_h - th) / 2.);
        pangocairo::functions::show_layout(cr, &l);
        Ok(())
    };

    for (i, label) in request.choices.iter().enumerate() {
        button(&cr, y, &choice_text(i, label), i == selected, true)?;
        hits.push((Hit::Choice(i), to_logical(pad, y, width, row_h)));
        y += row_h + gap;
    }
    y += gap - gap.min(gap);

    if let Some((label, _)) = &request.remember {
        let l = make_layout(&cr, &font, &remember_text(label), false);
        let th = f64::from(l.pixel_size().1);
        cr.set_source_rgba(1., 1., 1., 0.9);
        cr.move_to(pad, y + (row_h - th) / 2.);
        pangocairo::functions::show_layout(&cr, &l);
        hits.push((Hit::Remember, to_logical(pad, y, width, row_h)));
        y += row_h;
    }
    y += gap;

    button(
        &cr,
        y,
        "Cancel   <span alpha='60%'>(Esc)</span>",
        false,
        false,
    )?;
    hits.push((Hit::Cancel, to_logical(pad, y, width, row_h)));
    y += row_h + gap;

    cr.move_to(pad, y);
    let l = make_layout(&cr, &font, footer, true);
    pangocairo::functions::show_layout(&cr, &l);
    drop(cr);

    let data = surface.take_data().unwrap();
    let buffer = MemoryBuffer::new(
        data.to_vec(),
        Fourcc::Argb8888,
        (total_w as i32, total_h as i32),
        scale,
        Transform::Normal,
    );
    Ok((buffer, hits))
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.);
    cr.arc(x + w - r, y + h - r, r, 0., FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3. * FRAC_PI_2);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use smithay::input::keyboard::Keysym;

    use super::*;

    fn request(
        choices: &[&str],
        remember: Option<bool>,
    ) -> (DialogRequest, async_channel::Receiver<DialogAnswer>) {
        let (tx, rx) = async_channel::bounded(1);
        (
            DialogRequest {
                title: String::from("Share your screen?"),
                body: String::from("An app wants to see your screen."),
                choices: choices.iter().map(|s| s.to_string()).collect(),
                remember: remember.map(|on| (String::from("Remember"), on)),
                reply: tx,
            },
            rx,
        )
    }

    #[test]
    fn keyboard_choice_and_remember() {
        let mut dialog = PortalDialog::default();
        let (req, rx) = request(&["Screen 1", "Screen 2"], Some(true));
        assert!(dialog.show(req));
        dialog.key(Keysym::Down);
        dialog.key(Keysym::r);
        dialog.key(Keysym::Return);
        assert!(!dialog.is_open());
        assert_eq!(
            rx.try_recv().unwrap(),
            DialogAnswer::Chosen {
                index: 1,
                remember: false
            }
        );
    }

    #[test]
    fn number_keys_escape_and_queue() {
        let mut dialog = PortalDialog::default();
        let (a, rx_a) = request(&["A", "B", "C"], None);
        let (b, rx_b) = request(&["X"], None);
        assert!(dialog.show(a));
        assert!(!dialog.show(b));
        dialog.key(Keysym::_3);
        assert_eq!(
            rx_a.try_recv().unwrap(),
            DialogAnswer::Chosen {
                index: 2,
                remember: false
            }
        );
        // The queued one is shown next.
        assert!(dialog.is_open());
        dialog.key(Keysym::Escape);
        assert_eq!(rx_b.try_recv().unwrap(), DialogAnswer::Cancelled);
        assert!(!dialog.is_open());
    }

    #[test]
    fn requester_gone_is_pruned() {
        let mut dialog = PortalDialog::default();
        let (a, rx_a) = request(&["A"], None);
        dialog.show(a);
        drop(rx_a);
        assert!(dialog.prune());
        assert!(!dialog.is_open());
    }

    #[test]
    fn renders_with_hit_rects() {
        let (req, _rx) = request(&["Screen 1", "A window…"], Some(false));
        for scale in [1., 4. / 3., 2.] {
            let (buffer, hits) = render(&req, 0, false, scale).unwrap();
            let size = buffer.logical_size();
            assert!(size.w >= MIN_WIDTH - 1.);
            // Two choices, the checkbox and Cancel, all inside the dialog and not overlapping.
            assert_eq!(hits.len(), 4);
            for (i, (_, a)) in hits.iter().enumerate() {
                assert!(a.loc.x >= 0. && a.loc.y >= 0.);
                assert!(a.loc.y + a.size.h <= size.h + 0.5);
                for (_, b) in &hits[i + 1..] {
                    assert!(!a.overlaps(*b));
                }
            }
        }
    }
}
