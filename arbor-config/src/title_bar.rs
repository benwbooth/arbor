//! arbor: server-side title bars.

use crate::utils::MergeWith;
use crate::{Color, FloatOrInt};

/// Title bars drawn by the compositor for windows using server-side decorations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleBars {
    pub off: bool,
    pub height: f64,
    pub active_color: Color,
    pub inactive_color: Color,
    pub active_text_color: Color,
    pub inactive_text_color: Color,
}

impl Default for TitleBars {
    fn default() -> Self {
        // Breeze Dark's header colors (Plasma 6).
        Self {
            off: false,
            height: 30.,
            active_color: Color::from_rgba8_unpremul(0x29, 0x2c, 0x30, 0xff),
            inactive_color: Color::from_rgba8_unpremul(0x20, 0x23, 0x26, 0xff),
            active_text_color: Color::from_rgba8_unpremul(0xfc, 0xfc, 0xfc, 0xff),
            inactive_text_color: Color::from_rgba8_unpremul(0xa1, 0xa9, 0xb1, 0xff),
        }
    }
}

impl MergeWith<TitleBarsPart> for TitleBars {
    fn merge_with(&mut self, part: &TitleBarsPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }
        if let Some(x) = part.height {
            self.height = x.0;
        }
        if let Some(x) = part.active_color {
            self.active_color = x;
        }
        if let Some(x) = part.inactive_color {
            self.inactive_color = x;
        }
        if let Some(x) = part.active_text_color {
            self.active_text_color = x;
        }
        if let Some(x) = part.inactive_text_color {
            self.inactive_text_color = x;
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct TitleBarsPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub height: Option<FloatOrInt<1, 200>>,
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub active_text_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_text_color: Option<Color>,
}
