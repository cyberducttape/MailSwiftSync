//! Guided OAuth application registration for Google Workspace and
//! Microsoft 365.
//!
//! MailSwiftSync does not ship its own OAuth client: each organization
//! registers an application in its own Google Cloud project or Entra ID
//! tenant and gives MailSwiftSync the client ID. This module states exactly
//! what that registration needs (it must agree with the provider profiles in
//! `oauth_authorize`), validates what the operator enters, and remembers the
//! non-secret client ID and tenant of the last successful sign-in so later
//! accounts need no retyping. Client secrets and tokens are never stored
//! here.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

const MAX_REGISTRATION_FILE_BYTES: u64 = 16 * 1024;

/// One value the operator copies into the provider's console.
pub(crate) struct RegistrationValue {
    pub(crate) label: &'static str,
    pub(crate) value: &'static str,
}

/// The one-time registration a provider requires.
pub(crate) struct RegistrationGuide {
    pub(crate) console_url: &'static str,
    pub(crate) steps: &'static [&'static str],
    pub(crate) values: &'static [RegistrationValue],
}

const GOOGLE_STEPS: &[&str] = &[
    "Open the Google Cloud console for a project owned by the organization.",
    "Configure the OAuth consent screen. For a Workspace organization choose Internal so only its own users can sign in.",
    "Add the Gmail scope below to the consent screen.",
    "Create an OAuth client ID of type Desktop app. Desktop clients accept the local loopback redirect MailSwiftSync uses on any port, so no redirect URI needs to be entered.",
    "Copy the client ID (it ends in .apps.googleusercontent.com) and the client secret Google shows for desktop clients.",
    "A Workspace administrator may also need to allow the application in the Admin console under API controls.",
];

const GOOGLE_VALUES: &[RegistrationValue] = &[
    RegistrationValue {
        label: "Application type",
        value: "Desktop app",
    },
    RegistrationValue {
        label: "Scope",
        value: "https://mail.google.com/",
    },
    RegistrationValue {
        label: "Loopback redirect, any port (nothing to enter)",
        value: "http://127.0.0.1",
    },
];

const MICROSOFT_STEPS: &[&str] = &[
    "In the Microsoft Entra admin center open App registrations and create a New registration in the organization's tenant.",
    "Under Authentication add the platform Mobile and desktop applications with the redirect URI below. Entra ignores the port for localhost redirects.",
    "Under Authentication enable Allow public client flows.",
    "Under API permissions add the delegated permissions below and grant admin consent if the tenant requires it.",
    "Make sure IMAP is enabled for each mailbox in Exchange Online; otherwise sign-in succeeds but IMAP reports that the user is authenticated but not connected.",
    "Copy the Application (client) ID and the Directory (tenant) ID from the Overview page.",
];

const MICROSOFT_VALUES: &[RegistrationValue] = &[
    RegistrationValue {
        label: "Platform",
        value: "Mobile and desktop applications",
    },
    RegistrationValue {
        label: "Redirect URI",
        value: "http://localhost",
    },
    RegistrationValue {
        label: "Permission",
        value: "Office 365 Exchange Online › IMAP.AccessAsUser.All (delegated)",
    },
    RegistrationValue {
        label: "Permission",
        value: "offline_access (delegated)",
    },
];

pub(crate) fn registration_guide(provider: &str) -> Option<RegistrationGuide> {
    match provider {
        "google" => Some(RegistrationGuide {
            console_url: "https://console.cloud.google.com/apis/credentials",
            steps: GOOGLE_STEPS,
            values: GOOGLE_VALUES,
        }),
        "microsoft" => Some(RegistrationGuide {
            console_url: "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade",
            steps: MICROSOFT_STEPS,
            values: MICROSOFT_VALUES,
        }),
        _ => None,
    }
}

fn is_guid(value: &str) -> bool {
    let groups = value.split('-').collect::<Vec<_>>();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Explain what is wrong with a client ID, or `None` when it has the shape
/// the provider issues. An empty ID is reported as missing.
pub(crate) fn client_id_problem(provider: &str, client_id: &str) -> Option<&'static str> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Some("Enter the client ID from the application registration.");
    }
    match provider {
        "google" => {
            let valid = client_id
                .strip_suffix(".apps.googleusercontent.com")
                .and_then(|prefix| prefix.split_once('-'))
                .is_some_and(|(number, suffix)| {
                    !number.is_empty()
                        && number.bytes().all(|byte| byte.is_ascii_digit())
                        && !suffix.is_empty()
                        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                });
            (!valid).then_some(
                "A Google client ID looks like 123456789012-abc123.apps.googleusercontent.com.",
            )
        }
        "microsoft" => (!is_guid(client_id))
            .then_some("A Microsoft application (client) ID is a GUID such as 00000000-0000-0000-0000-000000000000."),
        _ => None,
    }
}

/// Explain what is wrong with a Microsoft tenant, or `None` when it is a
/// tenant ID, a verified domain, `organizations`, or `common`.
pub(crate) fn tenant_problem(tenant: &str) -> Option<&'static str> {
    let tenant = tenant.trim();
    if tenant.is_empty() {
        return Some("Enter the Directory (tenant) ID or a verified domain of the organization.");
    }
    let named = matches!(tenant, "organizations" | "common");
    let domain = tenant.contains('.')
        && !tenant.starts_with('.')
        && !tenant.ends_with('.')
        && tenant
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'));
    (!(named || domain || is_guid(tenant)))
        .then_some("Use the Directory (tenant) ID, a verified domain such as contoso.com, organizations, or common.")
}

/// Non-secret client registration remembered after a successful sign-in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClientRegistration {
    pub(crate) client_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) tenant: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClientRegistrations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) google: Option<ClientRegistration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) microsoft: Option<ClientRegistration>,
}

impl ClientRegistrations {
    pub(crate) fn path() -> PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mailswiftsync/oauth-clients.toml")
    }

    pub(crate) fn load() -> Self {
        Self::load_from(&Self::path())
    }

    fn load_from(path: &std::path::Path) -> Self {
        (|| {
            let file = std::fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() > MAX_REGISTRATION_FILE_BYTES {
                return None;
            }
            let mut text = String::new();
            file.take(MAX_REGISTRATION_FILE_BYTES)
                .read_to_string(&mut text)
                .ok()?;
            toml::from_str::<Self>(&text).ok()
        })()
        .unwrap_or_default()
    }

    pub(crate) fn get(&self, provider: &str) -> Option<&ClientRegistration> {
        match provider {
            "google" => self.google.as_ref(),
            "microsoft" => self.microsoft.as_ref(),
            _ => None,
        }
    }

    /// Remember a registration that just completed a sign-in.
    pub(crate) fn remember(&mut self, provider: &str, registration: ClientRegistration) {
        match provider {
            "google" => self.google = Some(registration),
            "microsoft" => self.microsoft = Some(registration),
            _ => {}
        }
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }

    fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            crate::credentials::ensure_private_directory(parent)
                .map_err(|error| error.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        crate::atomic_artifact::write_private_atomic(path, &content)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_match_the_values_the_authorizer_uses() {
        let google =
            crate::oauth_authorize::provider_profile("google", Default::default()).unwrap();
        let guide = registration_guide("google").unwrap();
        assert!(guide.values.iter().any(|value| value.value == google.scope));
        let microsoft = crate::oauth_authorize::provider_profile(
            "microsoft",
            crate::oauth_authorize::ProviderOverrides {
                tenant: Some("organizations".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let guide = registration_guide("microsoft").unwrap();
        for permission in ["IMAP.AccessAsUser.All", "offline_access"] {
            assert!(microsoft.scope.contains(permission));
            assert!(
                guide
                    .values
                    .iter()
                    .any(|value| value.value.contains(permission))
            );
        }
        assert!(
            guide
                .values
                .iter()
                .any(|value| value.value == "http://localhost")
        );
        assert!(registration_guide("custom").is_none());
    }

    #[test]
    fn client_ids_and_tenants_are_checked_against_the_issued_shapes() {
        assert_eq!(
            client_id_problem(
                "google",
                " 123456789012-abc123def.apps.googleusercontent.com "
            ),
            None
        );
        assert!(client_id_problem("google", "00000000-0000-0000-0000-000000000000").is_some());
        assert!(client_id_problem("google", "").is_some());
        assert_eq!(
            client_id_problem("microsoft", "6731de76-14a6-49ae-97bc-6eba6914391e"),
            None
        );
        assert!(client_id_problem("microsoft", "123-abc.apps.googleusercontent.com").is_some());
        for tenant in [
            "organizations",
            "common",
            "contoso.com",
            "6731de76-14a6-49ae-97bc-6eba6914391e",
        ] {
            assert_eq!(tenant_problem(tenant), None, "{tenant}");
        }
        for tenant in ["", "contoso", ".com", "bad tenant"] {
            assert!(tenant_problem(tenant).is_some(), "{tenant}");
        }
    }

    #[test]
    fn registrations_round_trip_without_secrets() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-oauth-{}", uuid::Uuid::new_v4()));
        let path = directory.join("mailswiftsync/oauth-clients.toml");
        let mut registrations = ClientRegistrations::default();
        registrations.remember(
            "microsoft",
            ClientRegistration {
                client_id: "6731de76-14a6-49ae-97bc-6eba6914391e".into(),
                tenant: "contoso.com".into(),
            },
        );
        std::fs::create_dir_all(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        registrations.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret"));
        let loaded = ClientRegistrations::load_from(&path);
        assert_eq!(loaded, registrations);
        assert_eq!(loaded.get("microsoft").unwrap().tenant, "contoso.com");
        assert!(loaded.get("google").is_none());
        let _ = std::fs::remove_dir_all(directory);
    }
}
