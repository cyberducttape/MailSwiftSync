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
    pub(crate) const ALL: [Self; 5] = [
        Self::GenericImap,
        Self::GoogleWorkspace,
        Self::Microsoft365,
        Self::Fastmail,
        Self::ZohoMail,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::GenericImap => "Generic IMAP preset",
            Self::GoogleWorkspace => "Google Workspace preset",
            Self::Microsoft365 => "Microsoft 365 preset",
            Self::Fastmail => "Fastmail preset",
            Self::ZohoMail => "Zoho Mail preset",
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
            Self::GoogleWorkspace => ProviderDefaults {
                host: "imap.gmail.com",
                port: "993",
                tls: "imaps",
                auth: "oauth2",
                note: "IMAP access and OAuth scope must be enabled by the Google Workspace administrator.",
            },
            Self::Microsoft365 => ProviderDefaults {
                host: "outlook.office365.com",
                port: "993",
                tls: "imaps",
                auth: "oauth2",
                note: "MailSwiftSync does not perform provider consent. Automatic refresh is available when an operator supplies a registered client and refresh token. Exchange Online IMAP and tenant OAuth policy must permit the account.",
            },
            Self::Fastmail => ProviderDefaults {
                host: "imap.fastmail.com",
                port: "993",
                tls: "imaps",
                auth: "password",
                note: "Fastmail commonly requires an app password; confirm the account's IMAP policy before preflight.",
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
        assert_eq!(ProviderPreset::ALL.len(), 5);
        assert!(ProviderPreset::GoogleWorkspace.label().ends_with(" preset"));
        assert!(ProviderPreset::Microsoft365.label().ends_with(" preset"));
    }
}
