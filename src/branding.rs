//! Operator/agency branding for customer-facing report exports.
//!
//! Set once per install, independent of any migration plan/profile: it is
//! not part of the plan fingerprint and has no effect on execution or
//! preflight/live gating. It exists only to let an MSP or hosting admin put
//! their own name and contact line on the customer-proof artifact instead of
//! (or alongside) MailSwiftSync's own. A missing or unreadable branding file
//! is not an error — an unbranded report is a completely valid customer-proof
//! export — so this loads best-effort with empty defaults, the same pattern
//! `ui::theme::AppearancePreferences` already uses for a non-secret,
//! per-install preference.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OperatorBranding {
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) contact: String,
}

impl OperatorBranding {
    pub(crate) fn is_empty(&self) -> bool {
        self.name.trim().is_empty() && self.contact.trim().is_empty()
    }

    pub(crate) fn path() -> PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/branding.toml")
    }

    pub(crate) fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .unwrap_or_default()
    }

    #[allow(dead_code)]
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
mod tests {
    use super::*;

    #[test]
    fn default_branding_is_empty() {
        assert!(OperatorBranding::default().is_empty());
    }

    #[test]
    fn branding_with_only_whitespace_is_still_empty() {
        let branding = OperatorBranding {
            name: "   ".into(),
            contact: "\t".into(),
        };
        assert!(branding.is_empty());
    }

    #[test]
    fn branding_with_a_name_is_not_empty() {
        let branding = OperatorBranding {
            name: "Acme Managed Services".into(),
            contact: String::new(),
        };
        assert!(!branding.is_empty());
    }

    #[test]
    fn branding_round_trips_through_toml() {
        let branding = OperatorBranding {
            name: "Acme Managed Services".into(),
            contact: "support@acme.example".into(),
        };
        let encoded = toml::to_string_pretty(&branding).unwrap();
        let decoded: OperatorBranding = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, branding);
    }

    #[test]
    fn missing_fields_decode_to_empty_strings() {
        let decoded: OperatorBranding = toml::from_str("").unwrap();
        assert_eq!(decoded, OperatorBranding::default());
    }
}
