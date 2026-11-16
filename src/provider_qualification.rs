//! Provider-pair qualification status loaded from the canonical capability
//! manifest. Provider presets are endpoint hints; only this evidence registry
//! can label a route qualified.

use crate::provider::ProviderPreset;
use serde::Deserialize;
use std::sync::OnceLock;

const CAPABILITIES: &str = include_str!("../capabilities.toml");

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    provider_qualifications: Vec<QualificationRecord>,
    #[serde(default)]
    qualification_invalidation_conditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct QualificationRecord {
    pair: String,
    source_provider: String,
    destination_provider: String,
    status: String,
    last_qualified: String,
    tested_engine: String,
    #[serde(default)]
    evidence_digest: String,
    limitations: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderQualificationStatus {
    pub(crate) pair: String,
    pub(crate) status: QualificationState,
    pub(crate) last_qualified: Option<String>,
    pub(crate) tested_engine: Option<String>,
    pub(crate) evidence_digest: Option<String>,
    pub(crate) limitations: String,
    pub(crate) invalidation_conditions: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QualificationState {
    Qualified,
    GenericLabOnly,
    NotQualified,
    Unknown,
}

static REGISTRY: OnceLock<Result<RegistryFile, String>> = OnceLock::new();

fn registry() -> Option<&'static RegistryFile> {
    REGISTRY
        .get_or_init(|| toml::from_str(CAPABILITIES).map_err(|error| error.to_string()))
        .as_ref()
        .ok()
}

pub(crate) fn for_presets(
    source: ProviderPreset,
    destination: ProviderPreset,
) -> ProviderQualificationStatus {
    let source = source.qualification_id();
    let destination = destination.qualification_id();
    let Some(registry) = registry() else {
        return unknown_status(source, destination);
    };
    let invalidation_conditions = registry.qualification_invalidation_conditions.clone();
    let Some(record) = registry.provider_qualifications.iter().find(|record| {
        record.source_provider == source && record.destination_provider == destination
    }) else {
        return ProviderQualificationStatus {
            pair: format!("{source} → {destination}"),
            status: QualificationState::Unknown,
            last_qualified: None,
            tested_engine: None,
            evidence_digest: None,
            limitations: "No provider-pair qualification record exists.".to_owned(),
            invalidation_conditions,
        };
    };

    status_for_record(record, invalidation_conditions)
}

pub(crate) fn for_endpoints(source: &str, destination: &str) -> ProviderQualificationStatus {
    match (
        ProviderPreset::infer_from_host(source),
        ProviderPreset::infer_from_host(destination),
    ) {
        (Some(source), Some(destination)) => for_presets(source, destination),
        _ => unknown_status(source, destination),
    }
}

fn status_for_record(
    record: &QualificationRecord,
    invalidation_conditions: Vec<String>,
) -> ProviderQualificationStatus {
    let digest = record.evidence_digest.trim().to_ascii_lowercase();
    let has_digest = digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    let has_date = !record.last_qualified.trim().is_empty()
        && record.last_qualified != "none"
        && record.last_qualified != "generic lab fixtures";
    let has_engine = !record.tested_engine.trim().is_empty();
    let state = match record.status.as_str() {
        "qualified" if has_digest && has_date && has_engine => QualificationState::Qualified,
        "qualified" => QualificationState::Unknown,
        "generic_lab_only" => QualificationState::GenericLabOnly,
        "not_qualified" => QualificationState::NotQualified,
        _ => QualificationState::Unknown,
    };
    ProviderQualificationStatus {
        pair: record.pair.clone(),
        status: state,
        last_qualified: has_date.then(|| record.last_qualified.clone()),
        tested_engine: has_engine.then(|| record.tested_engine.clone()),
        evidence_digest: has_digest.then_some(digest),
        limitations: record.limitations.clone(),
        invalidation_conditions,
    }
}

fn unknown_status(source: &str, destination: &str) -> ProviderQualificationStatus {
    ProviderQualificationStatus {
        pair: format!("{source} → {destination}"),
        status: QualificationState::Unknown,
        last_qualified: None,
        tested_engine: None,
        evidence_digest: None,
        limitations: "The provider qualification registry could not be loaded.".to_owned(),
        invalidation_conditions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        QualificationRecord, QualificationState, for_endpoints, for_presets, status_for_record,
    };
    use crate::provider::ProviderPreset;

    #[test]
    fn manifest_status_is_used_for_all_declared_routes_and_unknown_routes_fail_closed() {
        let hosting = for_presets(ProviderPreset::CpanelDovecot, ProviderPreset::CpanelDovecot);
        assert_eq!(hosting.pair, "cPanel/Dovecot → cPanel/Dovecot");
        assert_eq!(hosting.status, QualificationState::NotQualified);
        assert_eq!(hosting.tested_engine.as_deref(), Some("imapsync 2.314"));
        assert!(hosting.evidence_digest.is_none());
        assert!(!hosting.invalidation_conditions.is_empty());

        let generic = for_presets(ProviderPreset::GenericImap, ProviderPreset::GenericImap);
        assert_eq!(generic.status, QualificationState::GenericLabOnly);

        let unlisted = for_presets(ProviderPreset::ZohoMail, ProviderPreset::Microsoft365);
        assert_eq!(unlisted.status, QualificationState::Unknown);
        assert!(unlisted.limitations.contains("No provider-pair"));

        let custom_hosts = for_endpoints("mail.customer.example", "mail.hosting.example");
        assert_eq!(custom_hosts.status, QualificationState::Unknown);
        assert_eq!(
            custom_hosts.pair,
            "mail.customer.example → mail.hosting.example"
        );
    }

    #[test]
    fn claimed_qualification_without_a_digest_and_date_is_not_green() {
        let record = super::QualificationRecord {
            pair: "test".into(),
            source_provider: "test".into(),
            destination_provider: "test".into(),
            status: "qualified".into(),
            last_qualified: "none".into(),
            tested_engine: "imapsync 2.314".into(),
            evidence_digest: String::new(),
            limitations: String::new(),
        };
        let status = status_for_record(&record, Vec::new());
        assert_eq!(status.status, QualificationState::Unknown);
    }

    #[test]
    fn qualified_status_requires_and_surfaces_provenance_fields() {
        let record = QualificationRecord {
            pair: "test → route".into(),
            source_provider: "test".into(),
            destination_provider: "route".into(),
            status: "qualified".into(),
            last_qualified: "2026-11-15".into(),
            tested_engine: "imapsync 2.314".into(),
            evidence_digest: "a".repeat(64),
            limitations: "Bounded fixture".into(),
        };
        let status = status_for_record(&record, vec!["release changes".into()]);
        assert_eq!(status.status, QualificationState::Qualified);
        assert_eq!(status.last_qualified.as_deref(), Some("2026-11-15"));
        assert_eq!(
            status.evidence_digest.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(status.invalidation_conditions, ["release changes"]);
    }
}
