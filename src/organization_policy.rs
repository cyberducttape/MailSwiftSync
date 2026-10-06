//! Organization-wide safety policy applied independently of an editable plan.
//!
//! The policy is intentionally small and declarative. It is loaded from the
//! owner-only configuration directory, shown during preflight, and rechecked
//! at batch admission so changing a profile cannot bypass an administrator's
//! safety boundary.

use crate::{Form, Profile};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

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
    /// Permit private, loopback, link-local, or local-only webhook targets,
    /// including addresses returned by DNS.
    pub(crate) allow_private_networks: bool,
    /// Optional host boundary. When non-empty, webhook hosts must match one entry.
    /// A `*.example.com` entry matches subdomains, but not `example.com` itself.
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
    /// Require encrypted source and destination transport.
    pub(crate) require_tls: bool,
    /// Defaults to true for backwards-compatible installations; set false
    /// alongside `require_tls` to make the intent explicit in policy files.
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
}

impl Default for OrganizationPolicy {
    fn default() -> Self {
        Self {
            require_tls: false,
            allow_plain_imap: true,
            allow_destination_deletion: true,
            minimum_verification: "aggregate".into(),
            max_concurrency: None,
            providers: BTreeMap::new(),
            webhooks: OrganizationWebhookPolicy::default(),
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

    /// Missing policy is the explicit permissive default. A present but
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
