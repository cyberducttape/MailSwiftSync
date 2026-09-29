use eframe::egui::{self, Color32, Stroke};
use serde::{Deserialize, Serialize};
use std::io::Read;

const MAX_APPEARANCE_FILE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum ThemeKind {
    #[default]
    Default,
    ClassicGreen,
    ClassicAmber,
    ClassicWhite,
    Retro80sNeon,
    HighContrast,
    TerminalBlue,
    Commodore64,
    Windows95,
    Windows31,
}

impl ThemeKind {
    pub(crate) fn all() -> &'static [Self] {
        &[
            Self::Default,
            Self::ClassicGreen,
            Self::ClassicAmber,
            Self::ClassicWhite,
            Self::Retro80sNeon,
            Self::HighContrast,
            Self::TerminalBlue,
            Self::Commodore64,
            Self::Windows95,
            Self::Windows31,
        ]
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::ClassicGreen => "Classic Green",
            Self::ClassicAmber => "Classic Amber",
            Self::ClassicWhite => "Classic White",
            Self::Retro80sNeon => "Retro 80s Neon",
            Self::HighContrast => "High Contrast",
            Self::TerminalBlue => "Terminal Blue",
            Self::Commodore64 => "Commodore 64",
            Self::Windows95 => "Windows 95",
            Self::Windows31 => "Windows 3.1",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ThemeColors {
    pub(crate) background: Color32,
    pub(crate) panel: Color32,
    pub(crate) window: Color32,
    pub(crate) text_primary: Color32,
    pub(crate) text_secondary: Color32,
    pub(crate) info: Color32,
    pub(crate) success: Color32,
    pub(crate) warning: Color32,
    pub(crate) danger: Color32,
    pub(crate) link: Color32,
    #[allow(dead_code)]
    pub(crate) selection: Color32,
    #[allow(dead_code)]
    pub(crate) border: Color32,
    /// Rounded, softly bordered surfaces for the flagship palettes. Retro and
    /// high-contrast packs keep square corners and full-strength borders.
    pub(crate) modern: bool,
}

impl ThemeColors {
    /// Install a complete context-level palette. Windows, menus, popups, and
    /// widgets all consume this shared `Visuals` object rather than inheriting
    /// a root `Ui`'s local overrides.
    pub(crate) fn visuals(self, dark_mode: bool) -> egui::Visuals {
        let mut visuals = if dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let stroke = |color| Stroke::new(1.0, color);
        let (widget_radius, window_radius) = if self.modern { (4, 10) } else { (3, 3) };
        let quiet_border = self.quiet_border();
        let widget = |bg_fill, weak_bg_fill, border, fg| egui::style::WidgetVisuals {
            bg_fill,
            weak_bg_fill,
            bg_stroke: stroke(border),
            corner_radius: egui::CornerRadius::same(widget_radius),
            fg_stroke: stroke(fg),
            expansion: 0.0,
        };
        // Resting buttons sit on a raised surface so they read as controls
        // without heavy outlines; hover and press use the accent.
        let resting = if self.modern { self.window } else { self.panel };
        visuals.dark_mode = dark_mode;
        visuals.override_text_color = Some(self.text_primary);
        visuals.weak_text_color = Some(self.text_secondary);
        visuals.widgets.noninteractive =
            widget(self.panel, self.panel, quiet_border, self.text_primary);
        visuals.widgets.inactive = widget(resting, resting, quiet_border, self.text_primary);
        visuals.widgets.hovered =
            widget(self.selection, self.selection, self.info, self.text_primary);
        // egui draws `strong()` text with the active foreground, so it must
        // stay the primary text colour; filled accent buttons pick their own
        // label colour (see `primary_button`).
        visuals.widgets.active = widget(self.info, self.info, self.info, self.text_primary);
        visuals.widgets.open = widget(self.selection, self.selection, self.info, self.text_primary);
        visuals.window_fill = self.window;
        visuals.window_stroke = stroke(self.border);
        visuals.window_corner_radius = egui::CornerRadius::same(window_radius);
        visuals.menu_corner_radius = egui::CornerRadius::same(widget_radius);
        visuals.panel_fill = self.panel;
        visuals.faint_bg_color = self.stripe();
        visuals.extreme_bg_color = self.background;
        visuals.text_edit_bg_color = Some(self.background);
        visuals.code_bg_color = self.background;
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.danger;
        visuals.hyperlink_color = self.link;
        visuals.selection.bg_fill = self.selection;
        visuals.selection.stroke = stroke(self.on_selection());
        visuals.popup_shadow.color = Color32::from_black_alpha(if dark_mode { 160 } else { 70 });
        visuals.button_frame = true;
        visuals.collapsing_header_frame = true;
        visuals.striped = true;
        visuals
    }

    /// Text on the selection background: the most legible of the primary
    /// text colour, white, or black. Retro packs select with saturated fills
    /// (Windows 95 navy) that primary text cannot sit on.
    pub(crate) fn on_selection(self) -> Color32 {
        [self.text_primary, Color32::WHITE, Color32::BLACK]
            .into_iter()
            .max_by(|left, right| {
                contrast_ratio(*left, self.selection)
                    .total_cmp(&contrast_ratio(*right, self.selection))
            })
            .unwrap_or(self.text_primary)
    }

    /// Table striping: a quiet tint that primary text stays legible on.
    /// Saturated selection fills (Windows 95 navy) must never be used here.
    pub(crate) fn stripe(self) -> Color32 {
        if self.modern {
            lerp_color(self.panel, self.selection, 0.45)
        } else {
            // Tint away from the text colour so the stripe can only raise
            // contrast: lighter under dark text, darker under light text.
            let away = if contrast_ratio(Color32::WHITE, self.text_primary)
                >= contrast_ratio(Color32::BLACK, self.text_primary)
            {
                Color32::WHITE
            } else {
                Color32::BLACK
            };
            lerp_color(self.background, away, 0.14)
        }
    }

    /// Border for cards, resting controls, and separators. Modern palettes
    /// soften it toward the panel colour; retro and high-contrast packs keep
    /// their full-strength border, which is part of their accessibility.
    pub(crate) fn quiet_border(self) -> Color32 {
        if self.modern {
            lerp_color(self.border, self.panel, 0.55)
        } else {
            self.border
        }
    }

    pub(crate) fn for_theme(theme: ThemeKind, dark_mode: bool) -> Self {
        match theme {
            ThemeKind::Default => {
                if dark_mode {
                    Self::dark()
                } else {
                    Self::light()
                }
            }
            ThemeKind::ClassicGreen => Self::classic_green(),
            ThemeKind::ClassicAmber => Self::classic_amber(),
            ThemeKind::ClassicWhite => Self::classic_white(),
            ThemeKind::Retro80sNeon => Self::retro_neon(),
            ThemeKind::HighContrast => Self::high_contrast(),
            ThemeKind::TerminalBlue => Self::terminal_blue(),
            ThemeKind::Commodore64 => Self::commodore64(),
            ThemeKind::Windows95 => Self::windows95(),
            ThemeKind::Windows31 => Self::windows31(),
        }
    }

    pub(crate) fn dark() -> Self {
        Self {
            background: Color32::from_rgb(10, 17, 28),
            panel: Color32::from_rgb(15, 23, 36),
            window: Color32::from_rgb(24, 35, 51),
            text_primary: Color32::from_rgb(238, 244, 251),
            text_secondary: Color32::from_rgb(184, 197, 214),
            info: Color32::from_rgb(117, 184, 255),
            success: Color32::from_rgb(88, 213, 192),
            warning: Color32::from_rgb(245, 193, 92),
            danger: Color32::from_rgb(255, 142, 130),
            link: Color32::from_rgb(140, 200, 255),
            selection: Color32::from_rgb(43, 62, 88),
            border: Color32::from_rgb(116, 139, 169),
            modern: true,
        }
    }
    pub(crate) fn light() -> Self {
        Self {
            background: Color32::from_rgb(235, 243, 252),
            panel: Color32::WHITE,
            window: Color32::WHITE,
            text_primary: Color32::from_rgb(17, 26, 43),
            text_secondary: Color32::from_rgb(66, 84, 106),
            info: Color32::from_rgb(7, 89, 166),
            success: Color32::from_rgb(0, 105, 92),
            warning: Color32::from_rgb(128, 91, 0),
            danger: Color32::from_rgb(161, 38, 26),
            link: Color32::from_rgb(7, 94, 175),
            selection: Color32::from_rgb(215, 230, 248),
            border: Color32::from_rgb(111, 132, 157),
            modern: true,
        }
    }

    fn classic_green() -> Self {
        Self::monochrome(
            Color32::from_rgb(0, 200, 0),
            Color32::from_rgb(0, 10, 0),
            Color32::from_rgb(0, 128, 0),
        )
    }

    fn classic_amber() -> Self {
        Self::monochrome(
            Color32::from_rgb(255, 191, 0),
            Color32::from_rgb(10, 8, 0),
            Color32::from_rgb(204, 153, 0),
        )
    }

    fn classic_white() -> Self {
        Self::monochrome(
            Color32::WHITE,
            Color32::from_rgb(10, 10, 10),
            Color32::from_rgb(190, 190, 190),
        )
    }

    fn monochrome(accent: Color32, background: Color32, border: Color32) -> Self {
        Self {
            background,
            panel: background,
            window: background,
            text_primary: accent,
            text_secondary: accent,
            info: accent,
            success: accent,
            warning: Color32::from_rgb(255, 235, 0),
            danger: Color32::from_rgb(255, 90, 60),
            link: accent,
            selection: Color32::from_rgb(35, 35, 35),
            border,
            modern: false,
        }
    }

    fn retro_neon() -> Self {
        Self {
            background: Color32::from_rgb(8, 0, 17),
            panel: Color32::from_rgb(17, 0, 34),
            window: Color32::from_rgb(28, 0, 52),
            text_primary: Color32::from_rgb(255, 255, 255),
            text_secondary: Color32::from_rgb(190, 190, 255),
            info: Color32::from_rgb(80, 160, 255),
            success: Color32::from_rgb(0, 255, 153),
            warning: Color32::from_rgb(255, 255, 0),
            danger: Color32::from_rgb(255, 0, 68),
            link: Color32::from_rgb(255, 100, 255),
            selection: Color32::from_rgb(68, 20, 90),
            border: Color32::from_rgb(68, 68, 255),
            modern: false,
        }
    }

    fn high_contrast() -> Self {
        Self {
            background: Color32::BLACK,
            panel: Color32::BLACK,
            window: Color32::from_rgb(20, 20, 20),
            text_primary: Color32::WHITE,
            text_secondary: Color32::from_rgb(220, 220, 220),
            info: Color32::from_rgb(100, 190, 255),
            success: Color32::from_rgb(0, 255, 0),
            warning: Color32::from_rgb(255, 255, 0),
            danger: Color32::from_rgb(255, 80, 80),
            link: Color32::from_rgb(130, 210, 255),
            selection: Color32::from_rgb(65, 65, 65),
            border: Color32::WHITE,
            modern: false,
        }
    }

    fn terminal_blue() -> Self {
        Self {
            background: Color32::from_rgb(0, 20, 40),
            panel: Color32::from_rgb(0, 20, 40),
            window: Color32::from_rgb(0, 40, 80),
            text_primary: Color32::WHITE,
            text_secondary: Color32::from_rgb(220, 235, 255),
            info: Color32::from_rgb(100, 180, 255),
            success: Color32::from_rgb(0, 255, 255),
            warning: Color32::from_rgb(255, 255, 0),
            danger: Color32::from_rgb(255, 100, 100),
            link: Color32::from_rgb(100, 190, 255),
            selection: Color32::from_rgb(0, 70, 125),
            border: Color32::from_rgb(0, 128, 255),
            modern: false,
        }
    }

    fn commodore64() -> Self {
        Self {
            background: Color32::from_rgb(0, 0, 170),
            panel: Color32::from_rgb(0, 0, 170),
            window: Color32::from_rgb(0, 0, 145),
            text_primary: Color32::WHITE,
            text_secondary: Color32::from_rgb(255, 204, 102),
            info: Color32::from_rgb(255, 221, 0),
            success: Color32::from_rgb(255, 187, 0),
            warning: Color32::WHITE,
            danger: Color32::from_rgb(255, 100, 100),
            link: Color32::from_rgb(255, 221, 0),
            selection: Color32::from_rgb(0, 0, 220),
            border: Color32::from_rgb(255, 187, 0),
            modern: false,
        }
    }

    fn windows95() -> Self {
        let navy = Color32::from_rgb(0, 0, 128);
        Self {
            // Keep the classic teal while giving black status text a full
            // normal-text contrast ratio on the app background.
            background: Color32::from_rgb(0, 140, 140),
            panel: Color32::from_rgb(192, 192, 192),
            window: Color32::from_rgb(166, 202, 240),
            text_primary: Color32::BLACK,
            text_secondary: Color32::from_rgb(35, 35, 35),
            info: Color32::BLACK,
            success: Color32::BLACK,
            warning: Color32::BLACK,
            danger: Color32::BLACK,
            link: Color32::BLACK,
            selection: navy,
            border: Color32::WHITE,
            modern: false,
        }
    }

    fn windows31() -> Self {
        Self {
            // The classic desktop teal is kept at a luminance where the
            // black normal-text status palette remains WCAG AA readable.
            background: Color32::from_rgb(0, 132, 132),
            panel: Color32::from_rgb(128, 128, 128),
            window: Color32::from_rgb(212, 208, 200),
            text_primary: Color32::BLACK,
            text_secondary: Color32::BLACK,
            info: Color32::BLACK,
            success: Color32::BLACK,
            warning: Color32::BLACK,
            danger: Color32::BLACK,
            link: Color32::BLACK,
            selection: Color32::from_rgb(0, 0, 128),
            border: Color32::WHITE,
            modern: false,
        }
    }
}

fn lerp_color(from: Color32, to: Color32, amount: f32) -> Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

/// Spacing and type scale shared by every page and dialog.
pub(crate) fn install_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.window_margin = egui::Margin::same(14);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.indent = 16.0;
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(20.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

pub(crate) fn contrast_ratio(foreground: Color32, background: Color32) -> f32 {
    fn luminance(color: Color32) -> f32 {
        let channel = |value: u8| {
            let value = f32::from(value) / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }
    let foreground = luminance(foreground);
    let background = luminance(background);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AppearancePreferences {
    #[serde(default)]
    pub(crate) theme: ThemeKind,
    #[serde(default)]
    pub(crate) language: super::UiLanguage,
    pub(crate) dark_mode: bool,
    pub(crate) ui_scale: f32,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Default,
            language: super::UiLanguage::English,
            dark_mode: true,
            ui_scale: crate::DEFAULT_UI_SCALE,
        }
    }
}

impl AppearancePreferences {
    pub(crate) fn path() -> PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/appearance.toml")
    }

    pub(crate) fn load() -> Self {
        let text = (|| {
            let file = std::fs::File::open(Self::path()).ok()?;
            if file.metadata().ok()?.len() > MAX_APPEARANCE_FILE_BYTES {
                return None;
            }
            let mut text = String::new();
            file.take(MAX_APPEARANCE_FILE_BYTES + 1)
                .read_to_string(&mut text)
                .ok()?;
            (text.len() as u64 <= MAX_APPEARANCE_FILE_BYTES).then_some(text)
        })();
        let preferences = text
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .unwrap_or_default();
        let ui_scale = if preferences.ui_scale.is_finite() {
            preferences
                .ui_scale
                .clamp(crate::MIN_UI_SCALE, crate::MAX_UI_SCALE)
        } else {
            crate::DEFAULT_UI_SCALE
        };
        Self {
            theme: preferences.theme,
            language: preferences.language,
            dark_mode: preferences.dark_mode,
            ui_scale,
        }
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            crate::credentials::ensure_private_directory(parent)
                .map_err(|error| error.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        crate::atomic_artifact::write_private_atomic(&path, &content)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
pub(crate) fn next_ui_scale(current: f32) -> f32 {
    const SCALES: [f32; 5] = [0.90, 1.00, 1.10, 1.25, 1.50];
    SCALES
        .iter()
        .copied()
        .find(|scale| *scale > current + f32::EPSILON)
        .unwrap_or(SCALES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_wayexpand_packs_and_windows_retro_packs() {
        assert_eq!(ThemeKind::all().len(), 10);
        assert_eq!(ThemeKind::Windows95.label(), "Windows 95");
        assert_eq!(ThemeKind::Windows31.label(), "Windows 3.1");
    }

    #[test]
    fn windows_retro_packs_have_distinct_surfaces() {
        let windows95 = ThemeColors::for_theme(ThemeKind::Windows95, true);
        let windows31 = ThemeColors::for_theme(ThemeKind::Windows31, true);
        assert_ne!(windows95.background, windows31.background);
        assert_ne!(windows95.panel, windows31.panel);
        assert_ne!(windows95.window, windows31.window);
    }

    #[test]
    fn every_pack_has_readable_primary_text() {
        for theme in ThemeKind::all() {
            let colors = ThemeColors::for_theme(*theme, true);
            assert!(
                contrast_ratio(colors.text_primary, colors.panel) >= 4.5,
                "{} primary text is too low contrast",
                theme.label()
            );
            assert!(
                contrast_ratio(colors.text_secondary, colors.panel) >= 4.5,
                "{} secondary text is too low contrast",
                theme.label()
            );
        }
    }

    #[test]
    fn semantic_colors_meet_wcag_aa_contrast_on_every_surface() {
        for theme in ThemeKind::all() {
            for dark_mode in [true, false] {
                let colors = ThemeColors::for_theme(*theme, dark_mode);
                let theme_name = theme.label();
                for (label, foreground) in [
                    ("info", colors.info),
                    ("success", colors.success),
                    ("warning", colors.warning),
                    ("danger", colors.danger),
                    ("link", colors.link),
                ] {
                    for (surface_name, surface) in [
                        ("background", colors.background),
                        ("panel", colors.panel),
                        ("window", colors.window),
                    ] {
                        assert!(
                            contrast_ratio(foreground, surface) >= 4.5,
                            "{theme_name} ({dark_mode}): {label} on {surface_name} is {:.2}:1",
                            contrast_ratio(foreground, surface)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn context_visuals_cover_interactive_and_popup_states() {
        for theme in ThemeKind::all() {
            let colors = ThemeColors::for_theme(*theme, false);
            let visuals = colors.visuals(false);
            assert_eq!(visuals.window_fill, colors.window);
            assert_eq!(visuals.panel_fill, colors.panel);
            assert_eq!(visuals.widgets.hovered.bg_fill, visuals.selection.bg_fill);
            assert_eq!(visuals.widgets.active.bg_fill, colors.info);
            assert_eq!(visuals.window_stroke.color, colors.border);
            assert!(visuals.button_frame && visuals.collapsing_header_frame);
        }
    }

    #[test]
    fn primary_text_is_legible_on_table_stripes() {
        for theme in ThemeKind::all() {
            for dark_mode in [true, false] {
                let colors = ThemeColors::for_theme(*theme, dark_mode);
                let ratio = contrast_ratio(colors.text_primary, colors.stripe());
                assert!(
                    ratio >= 4.5,
                    "{} ({dark_mode}): stripe text is {ratio:.2}:1",
                    theme.label()
                );
            }
        }
    }

    #[test]
    fn selected_text_is_legible_on_every_selection_fill() {
        for theme in ThemeKind::all() {
            for dark_mode in [true, false] {
                let colors = ThemeColors::for_theme(*theme, dark_mode);
                let ratio = contrast_ratio(colors.on_selection(), colors.selection);
                assert!(
                    ratio >= 4.5,
                    "{} ({dark_mode}): selected text is {ratio:.2}:1",
                    theme.label()
                );
            }
        }
    }

    #[test]
    fn context_zoom_changes_real_widget_spacing() {
        let context = egui::Context::default();
        context.set_zoom_factor(1.0);
        let _ = context.run_ui(Default::default(), |_| {});
        let normal_button_height = measure_scale_probe_button(&context);
        context.set_zoom_factor(1.5);
        let _ = context.run_ui(Default::default(), |_| {});
        let large_button_height = measure_scale_probe_button(&context);

        assert_ne!(normal_button_height, large_button_height);
        assert!(large_button_height > normal_button_height);
    }

    fn measure_scale_probe_button(context: &egui::Context) -> f32 {
        let mut height = 0.0;
        let _ = context.run_ui(Default::default(), |ui| {
            height = ui.button("Scale probe").rect.height();
        });
        height * context.pixels_per_point()
    }

    #[test]
    fn appearance_preferences_default_legacy_files_to_english_and_read_german() {
        let legacy: AppearancePreferences =
            toml::from_str("dark_mode = true\nui_scale = 1.0\n").unwrap();
        assert_eq!(legacy.language, super::super::UiLanguage::English);

        let german: AppearancePreferences =
            toml::from_str("language = 'German'\ndark_mode = true\nui_scale = 1.0\n").unwrap();
        assert_eq!(german.language, super::super::UiLanguage::German);
    }
}
