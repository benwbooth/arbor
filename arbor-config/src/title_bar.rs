//! arbor: server-side title bars.

use crate::utils::MergeWith;
use crate::{Color, FloatOrInt};

/// Title bars drawn by the compositor for windows using server-side decorations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleBars {
    pub off: bool,
    /// Light colors instead of dark (hearth-color-scheme sets it with hearth's mode).
    pub light: bool,
    pub height: f64,
    pub active_color: Color,
    pub inactive_color: Color,
    pub active_text_color: Color,
    pub inactive_text_color: Color,
}

impl Default for TitleBars {
    fn default() -> Self {
        Self {
            off: false,
            height: 30.,
            ..Self::breeze(false)
        }
    }
}

impl TitleBars {
    /// macOS-style colors in hearth's palette (dark or light): the bar is the window's own
    /// background (a hair lighter while active, so the focused window stands out), not a band
    /// of its own as in Breeze; the title is the label color at 85% while active and fades to
    /// about a third when inactive, as macOS's does.
    fn breeze(light: bool) -> Self {
        let c = |r, g, b, a| Color::from_rgba8_unpremul(r, g, b, a);
        let (active, inactive, active_text, inactive_text) = if light {
            (
                c(0xf6, 0xf6, 0xf7, 0xff),
                c(0xef, 0xf0, 0xf1, 0xff),
                c(0x00, 0x00, 0x00, 217),
                c(0x00, 0x00, 0x00, 77),
            )
        } else {
            (
                c(0x25, 0x28, 0x2b, 0xff),
                c(0x20, 0x23, 0x26, 0xff),
                c(0xff, 0xff, 0xff, 217),
                c(0xff, 0xff, 0xff, 89),
            )
        };
        Self {
            off: false,
            light,
            height: 30.,
            active_color: active,
            inactive_color: inactive,
            active_text_color: active_text,
            inactive_text_color: inactive_text,
        }
    }
}

impl MergeWith<TitleBarsPart> for TitleBars {
    fn merge_with(&mut self, part: &TitleBarsPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }
        // A scheme switch resets the colors; explicit colors in the same block still win.
        if (part.light && !self.light) || (part.dark && self.light) {
            let scheme = Self::breeze(part.light);
            self.light = scheme.light;
            self.active_color = scheme.active_color;
            self.inactive_color = scheme.inactive_color;
            self.active_text_color = scheme.active_text_color;
            self.inactive_text_color = scheme.inactive_text_color;
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
    /// Breeze Light colors.
    #[knuffel(child)]
    pub light: bool,
    /// Breeze Dark colors (the default; undoes `light` from an earlier file).
    #[knuffel(child)]
    pub dark: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_scheme_and_overrides() {
        let mut bars = TitleBars::default();
        let red = Color::from_rgba8_unpremul(0xff, 0, 0, 0xff);
        bars.merge_with(&TitleBarsPart {
            light: true,
            active_color: Some(red),
            ..Default::default()
        });
        assert!(bars.light);
        assert_eq!(bars.active_color, red);
        assert_eq!(bars.inactive_color, TitleBars::breeze(true).inactive_color);
        assert_eq!(
            bars.active_text_color,
            TitleBars::breeze(true).active_text_color
        );

        bars.merge_with(&TitleBarsPart {
            dark: true,
            ..Default::default()
        });
        assert_eq!(bars, TitleBars::default());
    }
}
