use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum ThemeKind {
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

impl Default for ThemeKind {
    fn default() -> Self {
        Self::Default
    }
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
    pub(crate) selection: Color32,
    pub(crate) border: Color32,
}

impl ThemeColors {
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
        }
    }

    fn terminal_blue() -> Self {
        Self {
            background: Color32::from_rgb(0, 20, 40),
            panel: Color32::from_rgb(0, 20, 40),
            window: Color32::from_rgb(0, 40, 80),
            text_primary: Color32::WHITE,
            text_secondary: Color32::from_rgb(220, 235, 255),
            info: Color32::from_rgb(65, 105, 225),
            success: Color32::from_rgb(0, 255, 255),
            warning: Color32::from_rgb(255, 255, 0),
            danger: Color32::from_rgb(255, 100, 100),
            link: Color32::from_rgb(100, 190, 255),
            selection: Color32::from_rgb(0, 70, 125),
            border: Color32::from_rgb(0, 128, 255),
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
        }
    }

    fn windows95() -> Self {
        let navy = Color32::from_rgb(0, 0, 128);
        Self {
            background: Color32::from_rgb(0, 128, 128),
            panel: Color32::from_rgb(192, 192, 192),
            window: Color32::from_rgb(192, 192, 192),
            text_primary: Color32::BLACK,
            text_secondary: Color32::from_rgb(35, 35, 35),
            info: navy,
            success: Color32::from_rgb(0, 100, 0),
            warning: Color32::from_rgb(128, 70, 0),
            danger: Color32::from_rgb(128, 0, 0),
            link: navy,
            selection: navy,
            border: Color32::WHITE,
        }
    }

    fn windows31() -> Self {
        Self {
            background: Color32::from_rgb(0, 128, 128),
            panel: Color32::from_rgb(170, 170, 170),
            window: Color32::from_rgb(192, 192, 192),
            text_primary: Color32::BLACK,
            text_secondary: Color32::from_rgb(40, 40, 40),
            info: Color32::from_rgb(0, 0, 128),
            success: Color32::from_rgb(0, 90, 0),
            warning: Color32::from_rgb(120, 65, 0),
            danger: Color32::from_rgb(128, 0, 0),
            link: Color32::from_rgb(0, 0, 128),
            selection: Color32::from_rgb(0, 0, 128),
            border: Color32::WHITE,
        }
    }
}

#[cfg(test)]
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
    pub(crate) dark_mode: bool,
    pub(crate) ui_scale: f32,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Default,
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
        let preferences = std::fs::read_to_string(Self::path())
            .ok()
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
            dark_mode: preferences.dark_mode,
            ui_scale,
        }
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            crate::restrict_directory_permissions(parent).map_err(|error| error.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        crate::atomic_artifact::write_private_atomic(&path, &content)
            .map_err(|error| error.to_string())
    }
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
