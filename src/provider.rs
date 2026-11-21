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

    #[cfg(test)]
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

    /// Stable identifier used by `capabilities.toml` qualification records.
    pub(crate) fn qualification_id(self) -> &'static str {
        match self {
            Self::GenericImap => "generic_imap",
            Self::CpanelDovecot => "cpanel_dovecot",
            Self::GoogleWorkspace => "google_workspace",
            Self::Microsoft365 => "microsoft365",
            Self::Fastmail => "fastmail",
            Self::ZohoMail => "zoho_mail",
        }
    }

    /// Parse the stable provider identifiers accepted by the qualification
    /// status CLI. Display labels and endpoint hostnames are intentionally not
    /// accepted as identifiers so automation remains unambiguous.
    pub(crate) fn from_qualification_id(value: &str) -> Option<Self> {
        match value {
            "generic_imap" => Some(Self::GenericImap),
            "cpanel_dovecot" => Some(Self::CpanelDovecot),
            "google_workspace" => Some(Self::GoogleWorkspace),
            "microsoft365" => Some(Self::Microsoft365),
            "fastmail" => Some(Self::Fastmail),
            "zoho_mail" => Some(Self::ZohoMail),
            _ => None,
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

/// A one-click endpoint hint derived from the account address the operator
/// typed. It is computed offline from the address alone: nothing is looked up
/// over the network and nothing leaves the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EndpointSuggestion {
    /// The address belongs to a hosted provider with a preset.
    Preset(ProviderPreset),
    /// A conventional `imap.<domain>` guess for any other domain. Preflight
    /// still decides whether it is right.
    Host(String),
}

/// Consumer and hosted domains whose IMAP endpoint is the preset's. Custom
/// domains hosted by these providers cannot be recognized offline and get the
/// `imap.<domain>` guess instead.
const PROVIDER_DOMAINS: &[(&str, ProviderPreset)] = &[
    ("gmail.com", ProviderPreset::GoogleWorkspace),
    ("googlemail.com", ProviderPreset::GoogleWorkspace),
    ("outlook.com", ProviderPreset::Microsoft365),
    ("hotmail.com", ProviderPreset::Microsoft365),
    ("live.com", ProviderPreset::Microsoft365),
    ("msn.com", ProviderPreset::Microsoft365),
    ("fastmail.com", ProviderPreset::Fastmail),
    ("fastmail.fm", ProviderPreset::Fastmail),
    ("zoho.com", ProviderPreset::ZohoMail),
    ("zohomail.com", ProviderPreset::ZohoMail),
];

/// Suggest server settings for an account address such as `ana@example.com`.
/// Returns `None` unless the text is a plausible address with a dotted DNS
/// domain, so plain user names (common for cPanel and Dovecot) get no hint.
pub(crate) fn suggest_endpoint_for_address(address: &str) -> Option<EndpointSuggestion> {
    let (local, domain) = address.trim().split_once('@')?;
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    if local.is_empty() || !valid_dns_domain(&domain) {
        return None;
    }
    if let Some((_, preset)) = PROVIDER_DOMAINS.iter().find(|(known, _)| *known == domain) {
        return Some(EndpointSuggestion::Preset(*preset));
    }
    Some(EndpointSuggestion::Host(format!("imap.{domain}")))
}

fn valid_dns_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        && labels
            .last()
            .is_some_and(|tld| !tld.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{EndpointSuggestion, ProviderPreset, suggest_endpoint_for_address};

    #[test]
    fn account_addresses_suggest_presets_or_a_conventional_imap_host() {
        assert_eq!(
            suggest_endpoint_for_address(" Ana@GMail.com "),
            Some(EndpointSuggestion::Preset(ProviderPreset::GoogleWorkspace))
        );
        assert_eq!(
            suggest_endpoint_for_address("bo@hotmail.com"),
            Some(EndpointSuggestion::Preset(ProviderPreset::Microsoft365))
        );
        assert_eq!(
            suggest_endpoint_for_address("cy@fastmail.fm."),
            Some(EndpointSuggestion::Preset(ProviderPreset::Fastmail))
        );
        assert_eq!(
            suggest_endpoint_for_address("dee@zohomail.com"),
            Some(EndpointSuggestion::Preset(ProviderPreset::ZohoMail))
        );
        assert_eq!(
            suggest_endpoint_for_address("info@Example.co.uk"),
            Some(EndpointSuggestion::Host("imap.example.co.uk".into()))
        );
        for no_hint in [
            "",
            "ana",
            "@example.com",
            "ana@",
            "ana@localhost",
            "ana@exa mple.com",
            "ana@-example.com",
            "ana@example..com",
            "ana@10.0.0.1",
            "a@b@example.com",
        ] {
            assert_eq!(suggest_endpoint_for_address(no_hint), None, "{no_hint}");
        }
    }

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

    #[test]
    fn every_stable_qualification_id_round_trips_and_unknown_ids_are_rejected() {
        for preset in ProviderPreset::ALL {
            assert_eq!(
                ProviderPreset::from_qualification_id(preset.qualification_id()),
                Some(preset)
            );
        }
        assert_eq!(
            ProviderPreset::from_qualification_id("Google Workspace"),
            None
        );
        assert_eq!(ProviderPreset::from_qualification_id("unknown"), None);
    }
}
