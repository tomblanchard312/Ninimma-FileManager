// Ninimma modification: independent project branding and integration.
//! Appearance: the colours every part of the window draws with, in one
//! place, and the choice between them (light, dark, high contrast, or
//! following the system), plus the interface size. Switching takes effect
//! on the next frame, without a restart.

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// What the user chose in View → Theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    /// Light or dark, as the system is set (dark where it can't tell).
    #[default]
    System,
    Light,
    Dark,
    /// White on black, yellow highlights.
    HighContrastDark,
    /// Black on white, dark blue highlights.
    HighContrastLight,
}

impl Appearance {
    pub const ALL: [Appearance; 5] = [
        Appearance::System,
        Appearance::Light,
        Appearance::Dark,
        Appearance::HighContrastDark,
        Appearance::HighContrastLight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Follow the system",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
            Appearance::HighContrastDark => "High contrast dark",
            Appearance::HighContrastLight => "High contrast light",
        }
    }

    /// The palette to use, given whether the system is in dark mode (None:
    /// it can't be told).
    pub fn palette(self, system_dark: Option<bool>) -> Palette {
        match self {
            Appearance::System if system_dark == Some(false) => LIGHT,
            Appearance::System | Appearance::Dark => DARK,
            Appearance::Light => LIGHT,
            Appearance::HighContrastDark => HIGH_CONTRAST_DARK,
            Appearance::HighContrastLight => HIGH_CONTRAST_LIGHT,
        }
    }
}

/// Interface sizes offered in View → Interface size.
pub const SCALES: [f32; 7] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0];

/// Every colour the window draws with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// The file list and main area.
    pub bg: Color32,
    /// The navigation pane, dialogs and tiles.
    pub sidebar: Color32,
    /// Column headers and buttons.
    pub header: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub selected_hover: Color32,
    /// Text and marks drawn on `selected` (a tick, a focused label).
    pub on_selected: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub accent: Color32,
    pub border: Color32,
    pub error: Color32,
    /// A drive nearly full; and the empty part of its bar.
    pub warning: Color32,
    pub track: Color32,
}

/// Today's look, unchanged.
pub const DARK: Palette = Palette {
    dark: true,
    bg: Color32::from_rgb(30, 30, 30),
    sidebar: Color32::from_rgb(38, 38, 38),
    header: Color32::from_rgb(45, 45, 45),
    hover: Color32::from_rgb(60, 60, 65),
    selected: Color32::from_rgb(0, 103, 192),
    selected_hover: Color32::from_rgb(30, 130, 210),
    on_selected: Color32::WHITE,
    text: Color32::from_rgb(245, 245, 245),
    text_secondary: Color32::from_rgb(180, 180, 180),
    accent: Color32::from_rgb(60, 150, 230),
    border: Color32::from_rgb(80, 80, 80),
    error: Color32::from_rgb(240, 110, 110),
    warning: Color32::from_rgb(220, 165, 60),
    track: Color32::from_rgb(70, 70, 70),
};

/// Light appearance: white list, light grey pane, blue selection.
pub const LIGHT: Palette = Palette {
    dark: false,
    bg: Color32::from_rgb(255, 255, 255),
    sidebar: Color32::from_rgb(243, 243, 243),
    header: Color32::from_rgb(232, 232, 232),
    hover: Color32::from_rgb(229, 239, 250),
    selected: Color32::from_rgb(204, 228, 247),
    selected_hover: Color32::from_rgb(184, 214, 240),
    on_selected: Color32::from_rgb(20, 20, 20),
    text: Color32::from_rgb(25, 25, 25),
    text_secondary: Color32::from_rgb(95, 95, 95),
    accent: Color32::from_rgb(0, 95, 184),
    border: Color32::from_rgb(200, 200, 200),
    error: Color32::from_rgb(196, 43, 28),
    warning: Color32::from_rgb(176, 110, 0),
    track: Color32::from_rgb(218, 218, 218),
};

/// Windows' "Night sky"-style contrast theme: white on black, yellow marks.
pub const HIGH_CONTRAST_DARK: Palette = Palette {
    dark: true,
    bg: Color32::BLACK,
    sidebar: Color32::BLACK,
    header: Color32::from_rgb(20, 20, 20),
    hover: Color32::from_rgb(0, 60, 120),
    selected: Color32::from_rgb(255, 255, 0),
    selected_hover: Color32::from_rgb(255, 235, 60),
    on_selected: Color32::BLACK,
    text: Color32::WHITE,
    text_secondary: Color32::WHITE,
    accent: Color32::from_rgb(255, 255, 0),
    border: Color32::WHITE,
    error: Color32::from_rgb(255, 120, 120),
    warning: Color32::from_rgb(255, 200, 0),
    track: Color32::from_rgb(90, 90, 90),
};

/// Windows' "Desert"-style contrast theme: black on white, dark blue marks.
pub const HIGH_CONTRAST_LIGHT: Palette = Palette {
    dark: false,
    bg: Color32::WHITE,
    sidebar: Color32::WHITE,
    header: Color32::from_rgb(235, 235, 235),
    hover: Color32::from_rgb(200, 225, 255),
    selected: Color32::from_rgb(0, 40, 140),
    selected_hover: Color32::from_rgb(20, 60, 170),
    on_selected: Color32::WHITE,
    text: Color32::BLACK,
    text_secondary: Color32::BLACK,
    accent: Color32::from_rgb(0, 40, 140),
    border: Color32::BLACK,
    error: Color32::from_rgb(170, 0, 0),
    warning: Color32::from_rgb(130, 70, 0),
    track: Color32::from_rgb(170, 170, 170),
};

thread_local! {
    // The UI runs on one thread (and each test on its own).
    static CURRENT: Cell<Palette> = const { Cell::new(DARK) };
    static APPLIED: Cell<Option<(Palette, f32)>> = const { Cell::new(None) };
}

/// The colours to draw with now.
pub fn pal() -> Palette {
    CURRENT.with(Cell::get)
}

/// Text and secondary text for something drawn on the selection colour
/// (`selected`) or on the normal background.
pub fn text_on(selected: bool) -> (Color32, Color32) {
    let p = pal();
    if selected {
        (p.on_selected, p.on_selected.gamma_multiply(0.85))
    } else {
        (p.text, p.text_secondary)
    }
}

/// Puts the chosen appearance and size into effect; cheap when nothing
/// changed. Call once a frame, before drawing.
pub fn apply(ctx: &egui::Context, appearance: Appearance, scale: f32) {
    let system_dark = ctx.system_theme().map(|t| t == egui::Theme::Dark);
    let palette = appearance.palette(system_dark);
    let scale = if scale.is_finite() {
        scale.clamp(0.5, 3.0)
    } else {
        1.0
    };
    CURRENT.with(|c| c.set(palette));
    if APPLIED.with(Cell::get) == Some((palette, scale)) {
        return;
    }
    APPLIED.with(|c| c.set(Some((palette, scale))));
    ctx.set_theme(if palette.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.all_styles_mut(|style| style_with(style, &palette));
    ctx.set_zoom_factor(scale);
}

fn style_with(style: &mut egui::Style, p: &Palette) {
    let v = &mut style.visuals;
    *v = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.sidebar;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.header;
    v.window_stroke = egui::Stroke::new(1.0, p.border);
    v.widgets.noninteractive.bg_fill = p.bg;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.text);
    for (w, fill) in [
        (&mut v.widgets.inactive, p.header),
        (&mut v.widgets.hovered, p.hover),
        (&mut v.widgets.active, p.selected),
        (&mut v.widgets.open, p.hover),
    ] {
        w.weak_bg_fill = fill;
        w.bg_fill = fill;
        w.fg_stroke = egui::Stroke::new(1.0, p.text);
    }
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, p.on_selected);
    // Contrast themes: every control has a visible edge, focus a thick one.
    if p.text_secondary == p.text {
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.bg_stroke = egui::Stroke::new(1.5, p.border);
        }
        v.widgets.hovered.bg_stroke = egui::Stroke::new(2.0, p.accent);
    }
    v.selection.bg_fill = p.selected;
    v.selection.stroke = egui::Stroke::new(1.0, p.on_selected);
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.error;
    v.warn_fg_color = p.warning;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast ratio between two colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let ch = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn every_theme_keeps_text_readable() {
        for (name, p, needed) in [
            ("dark", DARK, 4.5),
            ("light", LIGHT, 4.5),
            ("high contrast dark", HIGH_CONTRAST_DARK, 7.0),
            ("high contrast light", HIGH_CONTRAST_LIGHT, 7.0),
        ] {
            for (what, fg, bg) in [
                ("text", p.text, p.bg),
                ("text in the pane", p.text, p.sidebar),
                ("secondary text", p.text_secondary, p.bg),
                ("selected text", p.on_selected, p.selected),
                ("text on hover (tab close button)", p.text, p.hover),
            ] {
                let c = contrast(fg, bg);
                assert!(c >= needed, "{name}: {what} contrast {c:.1} < {needed}");
            }
        }
    }

    #[test]
    fn following_the_system_picks_light_or_dark() {
        assert_eq!(Appearance::System.palette(Some(false)), LIGHT);
        assert_eq!(Appearance::System.palette(Some(true)), DARK);
        assert_eq!(
            Appearance::System.palette(None),
            DARK,
            "today's look when unknown"
        );
        assert_eq!(
            Appearance::Light.palette(Some(true)),
            LIGHT,
            "a choice wins"
        );
    }

    #[test]
    fn switching_takes_effect_without_a_restart() {
        let ctx = egui::Context::default();
        apply(&ctx, Appearance::Light, 1.25);
        assert_eq!(pal(), LIGHT);
        // A new size is applied with the next frame.
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        assert_eq!(ctx.zoom_factor(), 1.25);
        assert_eq!(
            ctx.style_of(egui::Theme::Light).visuals.panel_fill,
            LIGHT.bg
        );
        apply(&ctx, Appearance::HighContrastDark, 1.0);
        assert_eq!(pal(), HIGH_CONTRAST_DARK);
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            Color32::BLACK
        );
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        assert_eq!(ctx.zoom_factor(), 1.0);
    }
}
