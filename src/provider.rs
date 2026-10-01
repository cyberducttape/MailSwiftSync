//! Provider connection presets used to reduce first-run configuration work.
//!
//! These are endpoint hints, not provider integrations. They never provision
//! accounts, request OAuth consent, or imply that a provider's policy permits
//! password authentication. Readiness discovery and the selected engine remain
//! authoritative.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProviderPreset {
    #[default]
    GenericImap,
    CpanelDovecot,
    GoogleWorkspace,
    Microsoft365,
    Fastmail,
    ZohoMail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderDefaults {
    pub(crate) host: &'static str,
    pub(crate) port: &'static str,
    pub(crate) tls: &'static str,
    pub(crate) auth: &'static str,
    pub(crate) note: &'static str,
}

impl ProviderPreset {
    pub(crate) const ALL: [Self; 6] = [
        Self::GenericImap,
        Self::CpanelDovecot,
        Self::GoogleWorkspace,
        Self::Microsoft365,
        Self::Fastmail,
        Self::ZohoMail,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GenericImap => "Generic IMAP preset",
            Self::CpanelDovecot => "cPanel / Dovecot preset",
            Self::GoogleWorkspace => "Google Workspace preset",
            Self::Microsoft365 => "Microsoft 365 preset",
            Self::Fastmail => "Fastmail preset",
            Self::ZohoMail => "Zoho Mail preset",
        }
    }

    /// Provider name for account cards (without the "preset" suffix).
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::GenericImap => "Other IMAP server",
            Self::CpanelDovecot => "cPanel / Dovecot",
            Self::GoogleWorkspace => "Google Workspace",
            Self::Microsoft365 => "Microsoft 365",
            Self::Fastmail => "Fastmail",
            Self::ZohoMail => "Zoho Mail",
        }
    }

    /// Recognize a provider from its preset IMAP endpoint so profiles loaded
    /// without a remembered preset still get the provider-specific card.
    pub(crate) fn infer_from_host(host: &str) -> Option<Self> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        Self::ALL.into_iter().find(|preset| {
            let preset_host = preset.defaults().host;
            !preset_host.is_empty() && host == preset_host
        })
    }

    pub(crate) fn runbook_name(self) -> &'static str {
        match self {
            Self::GenericImap => "generic",
            Self::CpanelDovecot => "generic",
            Self::GoogleWorkspace => "gmail",
            Self::Microsoft365 => "microsoft365",
            Self::Fastmail => "fastmail",
            Self::ZohoMail => "zoho",
        }
    }

    pub(crate) fn defaults(self) -> ProviderDefaults {
        match self {
            Self::GenericImap => ProviderDefaults {
                host: "",
                port: "993",
                tls: "imaps",
                auth: "password",
                note: "Enter the provider's documented IMAP endpoint and authentication policy.",
            },
            Self::CpanelDovecot => ProviderDefaults {
                host: "",
                port: "993",
                tls: "imaps",
                auth: "password",
                note: "Enter the cPanel mail server for this domain. cPanel endpoints vary by hosting provider; confirm the hostname in the hosting account.",
            },
            Self::GoogleWorkspace => ProviderDefaults {
                host: "imap.gmail.com",
                port: "993",
                tls: "imaps",
                auth: "oauth2",
                note: "OAuth is preferred; Workspace administrators may use delegated gmail.imap_admin access. Register your own OAuth client, then use Connect Google Workspace account below to store a refresh token.",
            },
            Self::Microsoft365 => ProviderDefaults {
                host: "outlook.office365.com",
                port: "993",
                tls: "imaps",
                auth: "oauth2",
                note: "Register your own OAuth application, then use Connect Microsoft 365 account below to store a refresh token; live launches refresh access tokens automatically. Exchange Online IMAP and tenant OAuth policy must permit the account.",
            },
            Self::Fastmail => ProviderDefaults {
                host: "imap.fastmail.com",
                port: "993",
                tls: "imaps",
                auth: "password",
                note: "Use a Fastmail app password, not the primary account password; create it in Fastmail security settings before preflight.",
            },
            Self::ZohoMail => ProviderDefaults {
                host: "imap.zoho.com",
                port: "993",
                tls: "imaps",
                auth: "password",
                note: "Zoho region and account policy may change the endpoint; confirm the documented IMAP host before preflight.",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderPreset;

    #[test]
    fn hosted_provider_endpoints_are_recognized_for_loaded_profiles() {
        assert_eq!(
            ProviderPreset::infer_from_host("IMAP.gmail.com."),
            Some(ProviderPreset::GoogleWorkspace)
        );
        assert_eq!(
            ProviderPreset::infer_from_host("outlook.office365.com"),
            Some(ProviderPreset::Microsoft365)
        );
        // Presets without a fixed endpoint never match, including empty hosts.
        assert_eq!(ProviderPreset::infer_from_host(""), None);
        assert_eq!(ProviderPreset::infer_from_host("imap.example.com"), None);
        for preset in ProviderPreset::ALL {
            assert!(!preset.display_name().contains("preset"));
        }
    }

    #[test]
    fn presets_are_explicit_hints_with_safe_defaults() {
        assert_eq!(
            ProviderPreset::GoogleWorkspace.defaults().host,
            "imap.gmail.com"
        );
        assert_eq!(ProviderPreset::GoogleWorkspace.defaults().auth, "oauth2");
        assert_eq!(
            ProviderPreset::Microsoft365.defaults().host,
            "outlook.office365.com"
        );
        assert_eq!(ProviderPreset::Fastmail.defaults().port, "993");
        assert_eq!(ProviderPreset::CpanelDovecot.defaults().port, "993");
        assert_eq!(ProviderPreset::ALL.len(), 6);
        assert!(ProviderPreset::GoogleWorkspace.label().ends_with(" preset"));
        assert!(ProviderPreset::Microsoft365.label().ends_with(" preset"));
        assert_eq!(ProviderPreset::GoogleWorkspace.runbook_name(), "gmail");
        assert_eq!(ProviderPreset::Microsoft365.runbook_name(), "microsoft365");
        assert_eq!(ProviderPreset::Fastmail.runbook_name(), "fastmail");
    }
}
