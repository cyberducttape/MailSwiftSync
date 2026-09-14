use eframe::egui::Color32;

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
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AppearancePreferences {
    pub(crate) dark_mode: bool,
    pub(crate) ui_scale: f32,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
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
        crate::write_private_atomic(&path, &content).map_err(|error| error.to_string())
    }
}
