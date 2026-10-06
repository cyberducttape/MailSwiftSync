//! Organization-wide safety policy applied independently of an editable plan.
//!
//! The policy is intentionally small and declarative. It is loaded from the
//! owner-only configuration directory, shown during preflight, and rechecked
//! at batch admission so changing a profile cannot bypass an administrator's
//! safety boundary.

use crate::{Form, Profile};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::{Duration, Instant},
};

// Unknown keys are refused: a misspelled safety key (`require_tsl`) must not
// silently fall back to the permissive default.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OrganizationProviderPolicy {
    /// Maximum worker concurrency allowed for one endpoint on this provider.
    pub(crate) max_concurrency: Option<usize>,
    /// Maximum worker concurrency allowed for a tenant on this provider.
    pub(crate) max_concurrency_per_tenant: Option<usize>,
    /// Maximum worker concurrency allowed for one credential on this provider.
    pub(crate) max_concurrency_per_credential: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OrganizationWebhookPolicy {
    /// Permit non-public, reserved, loopback, link-local, or local-only
    /// webhook targets, including addresses returned by DNS.
    pub(crate) allow_private_networks: bool,
    /// Optional host boundary. When non-empty, webhook hosts must match one entry.
    /// A `*.example.com` entry matches subdomains, but not `example.com` itself.
    pub(crate) allowed_domains: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OrganizationOAuthPolicy {
    /// Permit provider profiles or endpoint overrides outside the built-in
    /// Google and Microsoft endpoint set.
    pub(crate) allow_custom_endpoints: bool,
    /// Permit OAuth endpoints that resolve to non-public or local addresses.
    pub(crate) allow_private_networks: bool,
    /// Optional host boundary for custom OAuth endpoints.
    pub(crate) allowed_domains: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderRateCeilings {
    pub(crate) provider: BTreeMap<String, usize>,
    pub(crate) tenant: BTreeMap<String, usize>,
    pub(crate) credential: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OrganizationPolicy {
    /// `production`, `compatibility`, or `lab`. Production is the default and
    /// rejects attempts to weaken the baseline below.
    #[serde(default = "default_policy_profile")]
    pub(crate) profile: String,
    /// Require encrypted source and destination transport.
    pub(crate) require_tls: bool,
    /// Defaults to false; enabling plaintext IMAP is an explicit compatibility
    /// exception for legacy servers.
    pub(crate) allow_plain_imap: bool,
    /// Whether plans that can remove destination-only state are permitted.
    pub(crate) allow_destination_deletion: bool,
    /// `aggregate`, `metadata`, or `body`.
    pub(crate) minimum_verification: String,
    /// Optional organization-wide upper bound for concurrent workers.
    pub(crate) max_concurrency: Option<usize>,
    /// Provider-specific tenant ceilings. Keys accept the canonical provider
    /// names (`gmail`, `microsoft365`, `generic`) plus documented
    /// aliases such as `google` and `o365`.
    pub(crate) providers: BTreeMap<String, OrganizationProviderPolicy>,
    /// Egress restrictions for credential-free but customer-sensitive webhooks.
    pub(crate) webhooks: OrganizationWebhookPolicy,
    /// Egress restrictions for OAuth endpoints carrying authorization material.
    pub(crate) oauth: OrganizationOAuthPolicy,
}

impl Default for OrganizationPolicy {
    fn default() -> Self {
        Self {
            profile: default_policy_profile(),
            require_tls: true,
            allow_plain_imap: false,
            allow_destination_deletion: false,
            minimum_verification: "metadata".into(),
            max_concurrency: Some(8),
            providers: BTreeMap::new(),
            webhooks: OrganizationWebhookPolicy::default(),
            oauth: OrganizationOAuthPolicy::default(),
        }
    }
}

fn default_policy_profile() -> String {
    "production".into()
}

/// Resolve an HTTPS endpoint through the bounded resolver and validate every
/// returned address before a caller opens a connection. HTTP clients should
/// pin themselves to the returned set to avoid a second DNS answer bypassing
/// the policy. A caller handing the URL to an external browser can only
/// validate before browser launch; the browser's subsequent DNS and TLS
/// connection are outside this process's pinning boundary.
pub(crate) fn resolve_policy_checked_https_target(
    url: &reqwest::Url,
    allow_private_networks: bool,
    allowed_domains: &[String],
    label: &str,
) -> Result<Vec<SocketAddr>, String> {
    let host = url
        .host_str()
        .ok_or_else(|| format!("the {label} endpoint is missing a host"))?;
    validate_endpoint_host(url, allow_private_networks, allowed_domains, label)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("the {label} endpoint is missing a port"))?;
    let addresses = match host.parse::<IpAddr>() {
        Ok(address) => vec![SocketAddr::new(address, port)],
        Err(_) => crate::imap_probe::resolve_dns_with_deadline(
            &format!("{host}:{port}"),
            Instant::now() + Duration::from_secs(10),
            &|| false,
        )
        .map_err(|error| format!("could not resolve {label} endpoint host: {error}"))?,
    };
    if addresses.is_empty() {
        return Err(format!("{label} endpoint host resolved to no addresses"));
    }
    if !allow_private_networks
        && addresses
            .iter()
            .any(|address| !is_allowed_public_egress(address.ip()))
    {
        return Err(format!(
            "{label} endpoint host resolves to a non-public or local-only address disabled by organization policy"
        ));
    }
    Ok(addresses)
}

fn validate_endpoint_host(
    url: &reqwest::Url,
    allow_private_networks: bool,
    allowed_domains: &[String],
    label: &str,
) -> Result<(), String> {
    let host = url
        .host_str()
        .ok_or_else(|| format!("the {label} endpoint is missing a host"))?;
    let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();
    if !allowed_domains.is_empty()
        && !allowed_domains
            .iter()
            .any(|allowed| policy_domain_matches(&normalized_host, allowed))
    {
        return Err(format!(
            "{label} endpoint host {normalized_host:?} is outside the organization allowed_domains policy"
        ));
    }
    if !allow_private_networks
        && host
            .parse::<IpAddr>()
            .is_ok_and(|address| !is_allowed_public_egress(address))
    {
        return Err(format!(
            "{label} endpoint host resolves to a non-public or local-only address disabled by organization policy"
        ));
    }
    Ok(())
}

pub(crate) fn validate_oauth_endpoint_policy(
    url: &reqwest::Url,
    policy: &OrganizationOAuthPolicy,
    label: &str,
) -> Result<(), String> {
    validate_endpoint_host(
        url,
        policy.allow_private_networks,
        &policy.allowed_domains,
        label,
    )?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("the {label} endpoint is missing a host"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !policy.allow_custom_endpoints
        && !matches!(
            host.as_str(),
            "accounts.google.com" | "oauth2.googleapis.com" | "login.microsoftonline.com"
        )
    {
        return Err(format!(
            "{label} endpoint host {host:?} is not an approved built-in provider endpoint; enable oauth.allow_custom_endpoints for reviewed custom OAuth"
        ));
    }
    Ok(())
}

fn policy_domain_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }
    pattern
        .strip_prefix("*.")
        .map_or(host == pattern, |suffix| {
            host.ends_with(&format!(".{suffix}")) && host != suffix
        })
}

/// Return true only for globally routable unicast addresses. SSRF policy is
/// intentionally an allowlist: documentation, benchmarking, special-use,
/// and reserved ranges are not safe just because they are not RFC1918.
pub(crate) fn is_allowed_public_egress(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, _] = address.octets();
            first != 0
                && first < 224
                && !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !(first == 100 && (64..=127).contains(&second))
                && !(first == 192 && second == 0)
                && !(first == 192 && second == 0 && third == 2)
                && !(first == 192 && second == 88 && third == 99)
                && !(first == 198 && (18..=19).contains(&second))
                && !(first == 198 && second == 51 && third == 100)
                && !(first == 203 && second == 0 && third == 113)
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            let is_global_unicast_prefix = (segments[0] & 0xe000) == 0x2000;
            is_global_unicast_prefix
                && address.to_ipv4().is_none()
                && !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && (segments[0] != 0x2001 || segments[1] != 0x0db8)
                && !(segments[0] == 0x2001 && segments[1] == 0x0000)
                && !(segments[0] == 0x2001 && segments[1] == 0x0002)
                && !(segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0010)
        }
    }
}

impl OrganizationPolicy {
    pub(crate) fn path() -> Result<PathBuf, String> {
        dirs_next::config_dir()
            .map(|directory| directory.join("mailswiftsync/organization-policy.toml"))
            .ok_or_else(|| {
                "Cannot determine the organization policy directory; repair the OS profile."
                    .to_owned()
            })
    }

    /// Missing policy uses the production-safe baseline. A present but
    /// malformed policy fails closed rather than silently weakening policy.
    pub(crate) fn load() -> Result<Self, String> {
        let path = Self::path()?;
        if matches!(
            std::fs::symlink_metadata(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ) {
            return Ok(Self::default());
        }
        let text = crate::credentials::read_secret_file(&path)
            .map_err(|error| format!("could not read organization policy: {error}"))?;
        toml::from_str(text.as_str()).map_err(|error| {
            format!(
                "could not decode organization policy {}: {error}",
                path.display()
            )
        })
    }

    pub(crate) fn check(&self, profile: &Profile) -> Result<(), String> {
        let policy_profile = self.profile.trim().to_ascii_lowercase();
        if !matches!(
            policy_profile.as_str(),
            "production" | "compatibility" | "lab"
        ) {
            return Err(format!(
                "Organization policy has unsupported profile {:?}; use production, compatibility, or lab.",
                self.profile
            ));
        }
        if policy_profile == "production"
            && (!self.require_tls
                || self.allow_plain_imap
                || self.allow_destination_deletion
                || !matches!(
                    self.minimum_verification
                        .trim()
                        .to_ascii_lowercase()
                        .as_str(),
                    "metadata" | "body"
                )
                || self.max_concurrency.is_none()
                || self.webhooks.allow_private_networks
                || self.oauth.allow_private_networks
                || self.oauth.allow_custom_endpoints)
        {
            return Err(
                "Organization policy profile production requires TLS, no destination deletion, metadata-or-body verification, bounded concurrency, public webhook/OAuth egress, and built-in OAuth endpoints; select compatibility or lab to acknowledge weaker behavior."
                    .into(),
            );
        }
        let source_plain = profile.source_tls.eq_ignore_ascii_case("plain");
        let destination_plain = profile.destination_tls.eq_ignore_ascii_case("plain");
        if (self.require_tls || !self.allow_plain_imap) && (source_plain || destination_plain) {
            return Err(
                "Organization policy requires TLS for both source and destination; plain IMAP is prohibited."
                    .into(),
            );
        }
        if !self.allow_destination_deletion
            && profile
                .destination_mutation_policy()
                .may_remove_destination_state()
        {
            return Err(
                "Organization policy prohibits destination deletion or mirror replacement for this migration plan."
                    .into(),
            );
        }
        let minimum = self.minimum_verification.trim().to_ascii_lowercase();
        if !matches!(minimum.as_str(), "" | "aggregate" | "metadata" | "body") {
            return Err(format!(
                "Organization policy has unsupported minimum_verification {:?}; use aggregate, metadata, or body.",
                self.minimum_verification
            ));
        }
        if matches!(minimum.as_str(), "metadata" | "body")
            && (profile.engine == crate::core::Engine::Dovecot
                || profile.automap
                || profile.justfolders
                || profile.addheader
                || !profile.sync_internaldates
                || profile.allowsizemismatch)
        {
            return Err(format!(
                "Organization policy requires {} verification, but this plan cannot establish that evidence; choose an immutable metadata-preserving imapsync plan.",
                minimum
            ));
        }
        if minimum == "body" && !profile.body_hash_verification {
            return Err(
                "Organization policy requires bounded body verification, but the plan does not enable it."
                    .into(),
            );
        }
        if let Some(maximum) = self.max_concurrency
            && maximum == 0
        {
            return Err(
                "Organization policy max_concurrency must be at least 1 when configured.".into(),
            );
        } else if let Some(maximum) = self.max_concurrency
            && profile.batch_concurrency > maximum
        {
            return Err(format!(
                "Organization policy limits batch concurrency to {maximum}; current plan requests {}.",
                profile.batch_concurrency
            ));
        }
        for (configured_provider, provider_policy) in &self.providers {
            canonical_provider_key(configured_provider)?;
            for (field, maximum) in [
                ("max_concurrency", provider_policy.max_concurrency),
                (
                    "max_concurrency_per_tenant",
                    provider_policy.max_concurrency_per_tenant,
                ),
                (
                    "max_concurrency_per_credential",
                    provider_policy.max_concurrency_per_credential,
                ),
            ] {
                if maximum == Some(0) {
                    return Err(format!(
                        "Organization policy provider {configured_provider:?} {field} must be at least 1."
                    ));
                }
            }
        }
        Ok(())
    }

    /// Canonical ceilings passed to the runtime hierarchical limiter. If
    /// aliases configure the same provider more than once, use the strictest
    /// value so TOML ordering cannot weaken policy.
    pub(crate) fn provider_rate_ceilings(&self) -> Result<ProviderRateCeilings, String> {
        let mut ceilings = ProviderRateCeilings::default();
        for (configured_provider, provider_policy) in &self.providers {
            let provider = canonical_provider_key(configured_provider)?;
            for (field, target, maximum) in [
                (
                    "max_concurrency",
                    &mut ceilings.provider,
                    provider_policy.max_concurrency,
                ),
                (
                    "max_concurrency_per_tenant",
                    &mut ceilings.tenant,
                    provider_policy.max_concurrency_per_tenant,
                ),
                (
                    "max_concurrency_per_credential",
                    &mut ceilings.credential,
                    provider_policy.max_concurrency_per_credential,
                ),
            ] {
                if let Some(maximum) = maximum {
                    if maximum == 0 {
                        return Err(format!(
                            "Organization policy provider {configured_provider:?} {field} must be at least 1."
                        ));
                    }
                    target
                        .entry(provider.to_owned())
                        .and_modify(|current: &mut usize| *current = (*current).min(maximum))
                        .or_insert(maximum);
                }
            }
        }
        Ok(ceilings)
    }

    pub(crate) fn check_form(&self, form: &Form) -> Result<(), String> {
        self.check(&form.profile)
    }
}

/// Resolve a policy provider key to the identity `canonical_provider` assigns
/// endpoints. A key that no endpoint can ever carry is refused rather than
/// silently configuring a ceiling that never applies.
fn canonical_provider_key(value: &str) -> Result<&'static str, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "gmail" | "google" | "google_workspace" | "google-workspace" => Ok("gmail"),
        "microsoft365" | "microsoft_365" | "microsoft" | "o365" => Ok("microsoft365"),
        "generic" | "generic_imap" | "generic-imap" => Ok("generic"),
        "dovecot" => Err(
            "Organization policy provider \"dovecot\" is no longer supported: Dovecot is server software, not a provider identity, so self-hosted Dovecot endpoints are \"generic\". Move these ceilings to [providers.generic]."
                .into(),
        ),
        _ => Err(format!(
            "Organization policy has unsupported provider {value:?}; use gmail, microsoft365, or generic."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::OrganizationPolicy;
    use crate::{Form, core};

    #[test]
    fn default_policy_uses_production_safe_baseline() {
        let policy = OrganizationPolicy::default();
        assert_eq!(policy.profile, "production");
        assert!(policy.require_tls);
        assert!(!policy.allow_plain_imap);
        assert!(!policy.allow_destination_deletion);
        assert_eq!(policy.minimum_verification, "metadata");
        assert_eq!(policy.max_concurrency, Some(8));
    }

    #[test]
    fn production_profile_rejects_weakening_without_explicit_compatibility() {
        let policy = OrganizationPolicy {
            allow_destination_deletion: true,
            ..OrganizationPolicy::default()
        };
        let error = policy.check_form(&Form::default()).unwrap_err();
        assert!(error.contains("profile production"), "{error}");

        let policy = OrganizationPolicy {
            profile: "compatibility".into(),
            allow_destination_deletion: true,
            ..OrganizationPolicy::default()
        };
        assert!(policy.check_form(&Form::default()).is_ok());
    }

    #[test]
    fn policy_profile_names_are_fail_closed() {
        let policy = OrganizationPolicy {
            profile: "operator-test".into(),
            ..OrganizationPolicy::default()
        };
        let error = policy.check_form(&Form::default()).unwrap_err();
        assert!(
            error.contains("production, compatibility, or lab"),
            "{error}"
        );
    }

    #[test]
    fn policy_rejects_plain_transport_and_destructive_plans() {
        let mut form = Form::default();
        form.profile.source_tls = "plain".into();
        form.profile.delete2 = true;
        let policy = OrganizationPolicy {
            require_tls: true,
            allow_plain_imap: false,
            allow_destination_deletion: false,
            ..OrganizationPolicy::default()
        };
        let error = policy.check_form(&form).unwrap_err();
        assert!(error.contains("TLS"));
        form.profile.source_tls = "starttls".into();
        assert!(policy.check_form(&form).unwrap_err().contains("deletion"));
    }

    #[test]
    fn policy_requires_independent_metadata_or_body_evidence() {
        let mut form = Form::default();
        form.profile.engine = core::Engine::Dovecot;
        let policy = OrganizationPolicy {
            minimum_verification: "metadata".into(),
            ..OrganizationPolicy::default()
        };
        assert!(policy.check_form(&form).unwrap_err().contains("metadata"));
    }

    #[test]
    fn policy_rejects_invalid_concurrency_bound() {
        let form = Form::default();
        let policy = OrganizationPolicy {
            max_concurrency: Some(0),
            ..OrganizationPolicy::default()
        };
        assert!(policy.check_form(&form).unwrap_err().contains("at least 1"));
    }

    #[test]
    fn provider_domain_ceilings_do_not_reduce_independent_global_workers() {
        let mut form = Form::default();
        form.profile.source_host = "imap.gmail.com".into();
        form.profile.batch_concurrency = 8;
        let policy = OrganizationPolicy {
            providers: [(
                "google".into(),
                super::OrganizationProviderPolicy {
                    max_concurrency: Some(6),
                    max_concurrency_per_tenant: Some(4),
                    max_concurrency_per_credential: Some(2),
                },
            )]
            .into_iter()
            .collect(),
            ..OrganizationPolicy::default()
        };
        policy.check_form(&form).unwrap();
        let ceilings = policy.provider_rate_ceilings().unwrap();
        assert_eq!(ceilings.provider.get("gmail"), Some(&6));
        assert_eq!(ceilings.tenant.get("gmail"), Some(&4));
        assert_eq!(ceilings.credential.get("gmail"), Some(&2));
    }

    #[test]
    fn provider_policy_aliases_merge_to_the_strictest_ceiling() {
        let policy = OrganizationPolicy {
            providers: [
                (
                    "gmail".into(),
                    super::OrganizationProviderPolicy {
                        max_concurrency: Some(8),
                        max_concurrency_per_tenant: Some(5),
                        max_concurrency_per_credential: Some(4),
                    },
                ),
                (
                    "google".into(),
                    super::OrganizationProviderPolicy {
                        max_concurrency: Some(3),
                        max_concurrency_per_tenant: Some(2),
                        max_concurrency_per_credential: Some(1),
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..OrganizationPolicy::default()
        };
        let ceilings = policy.provider_rate_ceilings().unwrap();
        assert_eq!(ceilings.provider.get("gmail"), Some(&3));
        assert_eq!(ceilings.tenant.get("gmail"), Some(&2));
        assert_eq!(ceilings.credential.get("gmail"), Some(&1));
    }

    #[test]
    fn policy_rejects_unknown_provider_policy_keys() {
        let policy = OrganizationPolicy {
            providers: [(
                "typo-provider".into(),
                super::OrganizationProviderPolicy::default(),
            )]
            .into_iter()
            .collect(),
            ..OrganizationPolicy::default()
        };
        let error = policy.check_form(&Form::default()).unwrap_err();
        assert!(error.contains("unsupported provider"), "{error}");
    }

    #[test]
    fn misspelled_policy_keys_fail_closed_instead_of_defaulting() {
        for text in [
            "require_tsl = true\n",
            "[providers.gmail]\nmax_concurency = 1\n",
        ] {
            assert!(
                toml::from_str::<OrganizationPolicy>(text).is_err(),
                "{text:?} must not decode to a permissive policy"
            );
        }
        let policy: OrganizationPolicy =
            toml::from_str("require_tls = true\n[providers.google]\nmax_concurrency = 2\n")
                .unwrap();
        assert!(policy.require_tls);
    }

    #[test]
    fn oauth_policy_allows_only_built_in_hosts_by_default() {
        let policy = OrganizationPolicy::default();
        let built_in = reqwest::Url::parse("https://oauth2.googleapis.com/token").unwrap();
        let custom = reqwest::Url::parse("https://oauth.example.test/token").unwrap();
        super::validate_oauth_endpoint_policy(&built_in, &policy.oauth, "OAuth token").unwrap();
        let error = super::validate_oauth_endpoint_policy(&custom, &policy.oauth, "OAuth token")
            .unwrap_err();
        assert!(error.contains("allow_custom_endpoints"), "{error}");
    }

    #[test]
    fn oauth_policy_rejects_literal_private_targets_without_opt_in() {
        let mut policy = OrganizationPolicy::default();
        policy.oauth.allow_custom_endpoints = true;
        let private = reqwest::Url::parse("https://127.0.0.1/token").unwrap();
        let error = super::validate_oauth_endpoint_policy(&private, &policy.oauth, "OAuth token")
            .unwrap_err();
        assert!(error.contains("non-public"), "{error}");
    }

    #[test]
    fn public_egress_rejects_documentation_and_reserved_ranges() {
        for address in [
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "240.0.0.1",
            "2001:db8::1",
        ] {
            let address = address.parse().unwrap();
            assert!(!super::is_allowed_public_egress(address), "{address}");
        }
        for address in ["1.1.1.1", "2606:4700:4700::1111"] {
            let address = address.parse().unwrap();
            assert!(super::is_allowed_public_egress(address), "{address}");
        }
    }

    #[test]
    fn dovecot_provider_key_is_refused_with_a_migration_hint() {
        let policy = OrganizationPolicy {
            providers: [(
                "dovecot".into(),
                super::OrganizationProviderPolicy {
                    max_concurrency: Some(2),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..OrganizationPolicy::default()
        };
        let error = policy.provider_rate_ceilings().unwrap_err();
        assert!(error.contains("[providers.generic]"), "{error}");
        assert!(policy.check_form(&Form::default()).is_err());
    }
}
