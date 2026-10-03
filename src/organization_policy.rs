//! Organization-wide safety policy applied independently of an editable plan.
//!
//! The policy is intentionally small and declarative. It is loaded from the
//! owner-only configuration directory, shown during preflight, and rechecked
//! at batch admission so changing a profile cannot bypass an administrator's
//! safety boundary.

use crate::{Form, Profile};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default)]
pub(crate) struct OrganizationProviderPolicy {
    /// Maximum worker concurrency allowed for a tenant on this provider.
    pub(crate) max_concurrency_per_tenant: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
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
    /// names (`gmail`, `microsoft365`, `dovecot`, `generic`) plus documented
    /// aliases such as `google` and `o365`.
    pub(crate) providers: BTreeMap<String, OrganizationProviderPolicy>,
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
            let provider = canonical_provider_key(configured_provider).ok_or_else(|| {
                format!(
                    "Organization policy has unsupported provider {:?}; use gmail, microsoft365, dovecot, or generic.",
                    configured_provider
                )
            })?;
            if let Some(maximum) = provider_policy.max_concurrency_per_tenant {
                if maximum == 0 {
                    return Err(format!(
                        "Organization policy provider {configured_provider:?} max_concurrency_per_tenant must be at least 1."
                    ));
                }
                let source_provider =
                    crate::core::provider_intelligence::canonical_provider(&profile.source_host);
                let destination_provider = crate::core::provider_intelligence::canonical_provider(
                    &profile.destination_host,
                );
                if (source_provider == provider || destination_provider == provider)
                    && profile.batch_concurrency > maximum
                {
                    return Err(format!(
                        "Organization policy limits {provider} tenant concurrency to {maximum}; current plan requests {}.",
                        profile.batch_concurrency
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn check_form(&self, form: &Form) -> Result<(), String> {
        self.check(&form.profile)
    }
}

fn canonical_provider_key(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "gmail" | "google" | "google_workspace" | "google-workspace" => Some("gmail"),
        "microsoft365" | "microsoft_365" | "microsoft" | "o365" => Some("microsoft365"),
        "dovecot" => Some("dovecot"),
        "generic" | "generic_imap" | "generic-imap" => Some("generic"),
        _ => None,
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
    fn policy_applies_provider_tenant_concurrency_ceiling() {
        let mut form = Form::default();
        form.profile.source_host = "imap.gmail.com".into();
        form.profile.batch_concurrency = 8;
        let policy = OrganizationPolicy {
            providers: [(
                "google".into(),
                super::OrganizationProviderPolicy {
                    max_concurrency_per_tenant: Some(4),
                },
            )]
            .into_iter()
            .collect(),
            ..OrganizationPolicy::default()
        };
        let error = policy.check_form(&form).unwrap_err();
        assert!(error.contains("gmail tenant concurrency"), "{error}");
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
}
