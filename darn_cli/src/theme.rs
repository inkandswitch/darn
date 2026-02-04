//! Catppuccin Mocha color theme for cliclack.
//!
//! A soothing pastel theme based on the Catppuccin color scheme:
//! <https://github.com/catppuccin/catppuccin>

use cliclack::{Theme, ThemeState};
use console::Style;

/// Catppuccin Mocha color palette.
mod palette {
    /// Mauve - primary accent (#cba6f7)
    pub(super) const MAUVE: (u8, u8, u8) = (203, 166, 247);

    /// Green - success color (#a6e3a1)
    pub(super) const GREEN: (u8, u8, u8) = (166, 227, 161);

    /// Yellow - active/pending color (#f9e2af)
    pub(super) const YELLOW: (u8, u8, u8) = (249, 226, 175);

    /// Red - error color (#f38ba8)
    pub(super) const RED: (u8, u8, u8) = (243, 139, 168);

    /// Peach - warning color (#fab387)
    pub(super) const PEACH: (u8, u8, u8) = (250, 179, 135);

    /// Blue - info color (#89b4fa)
    pub(super) const BLUE: (u8, u8, u8) = (137, 180, 250);

    /// Text - foreground (#cdd6f4)
    pub(super) const TEXT: (u8, u8, u8) = (205, 214, 244);

    /// Overlay1 - dimmed text (#7f849c)
    pub(super) const OVERLAY1: (u8, u8, u8) = (127, 132, 156);
}

const fn style_from_rgb((r, g, b): (u8, u8, u8)) -> Style {
    Style::new().color256(ansi256_from_rgb(r, g, b))
}

/// Convert RGB to closest ANSI 256 color.
///
/// Uses the 6x6x6 color cube (indices 16-231) for best approximation.
const fn ansi256_from_rgb(r: u8, g: u8, b: u8) -> u8 {
    let r_idx = if r < 48 { 0 } else { (r - 35) / 40 };
    let g_idx = if g < 48 { 0 } else { (g - 35) / 40 };
    let b_idx = if b < 48 { 0 } else { (b - 35) / 40 };
    16 + 36 * r_idx + 6 * g_idx + b_idx
}

/// Catppuccin Mocha theme for cliclack prompts.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CatppuccinMocha;

impl CatppuccinMocha {
    fn mauve() -> Style {
        style_from_rgb(palette::MAUVE)
    }

    fn green() -> Style {
        style_from_rgb(palette::GREEN)
    }

    fn yellow() -> Style {
        style_from_rgb(palette::YELLOW)
    }

    fn red() -> Style {
        style_from_rgb(palette::RED)
    }

    fn peach() -> Style {
        style_from_rgb(palette::PEACH)
    }

    fn blue() -> Style {
        style_from_rgb(palette::BLUE)
    }

    fn text() -> Style {
        style_from_rgb(palette::TEXT)
    }

    fn overlay1() -> Style {
        style_from_rgb(palette::OVERLAY1)
    }
}

impl Theme for CatppuccinMocha {
    fn bar_color(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Active => Self::mauve(),
            ThemeState::Cancel | ThemeState::Error(_) => Self::red(),
            ThemeState::Submit => Self::green(),
        }
    }

    fn state_symbol_color(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Active => Self::yellow(),
            ThemeState::Cancel | ThemeState::Error(_) => Self::red(),
            ThemeState::Submit => Self::green(),
        }
    }

    fn input_style(&self, _state: &ThemeState) -> Style {
        Self::text()
    }

    fn placeholder_style(&self, _state: &ThemeState) -> Style {
        Self::overlay1()
    }

    fn info_symbol(&self) -> String {
        Self::blue().apply_to("●").to_string()
    }

    fn warning_symbol(&self) -> String {
        Self::peach().apply_to("▲").to_string()
    }

    fn error_symbol(&self) -> String {
        Self::red().apply_to("■").to_string()
    }

    fn remark_symbol(&self) -> String {
        Self::overlay1().apply_to("─").to_string()
    }
}

/// Apply the Catppuccin Mocha theme globally.
pub(crate) fn apply() {
    cliclack::set_theme(CatppuccinMocha);
}
