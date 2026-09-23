//! Background readiness probes shared by the GUI controller.
//!
//! The egui shell prepares an immutable request and receives only a bounded
//! result. Network I/O and secret-bearing worker ownership stay outside the
//! application update path.

use super::{CapabilityProbeResult, LiveAuthProof};
use crate::core;
use crate::credentials::SecretString;
use crate::imap_probe::{
    probe_tls_authentication_with_transport, probe_tls_capabilities_with_transport,
};
use std::sync::mpsc::{self, Receiver};

pub(crate) struct ImapProbeEndpoint {
    pub(crate) endpoint: String,
    pub(crate) user: String,
    pub(crate) password: SecretString,
    pub(crate) auth: String,
    pub(crate) tls: String,
    pub(crate) ca_bundle: String,
    pub(crate) certificate_pin_sha256: String,
}

pub(crate) struct CapabilityProbeSpec {
    pub(crate) request_id: String,
    pub(crate) plan_fingerprint: String,
    pub(crate) source: ImapProbeEndpoint,
    pub(crate) destination: ImapProbeEndpoint,
}

pub(crate) fn spawn_capability_probe(spec: CapabilityProbeSpec) -> Receiver<CapabilityProbeResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = probe_endpoint(&spec.source).and_then(|source| {
            probe_endpoint(&spec.destination).map(|destination| (source, destination))
        });
        let _ = tx.send(CapabilityProbeResult {
            request_id: spec.request_id,
            plan_fingerprint: spec.plan_fingerprint,
            result,
        });
    });
    rx
}

pub(crate) struct LiveAuthProbeSpec {
    pub(crate) plan_fingerprint: String,
    pub(crate) credential_fingerprint: String,
    pub(crate) source: ImapProbeEndpoint,
    pub(crate) destination: ImapProbeEndpoint,
}

pub(crate) fn spawn_live_auth_probe(
    spec: LiveAuthProbeSpec,
) -> Receiver<Result<LiveAuthProof, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = probe_auth_endpoint(&spec.source).and_then(|_| {
            probe_auth_endpoint(&spec.destination).map(|_| LiveAuthProof {
                plan_fingerprint: spec.plan_fingerprint,
                credential_fingerprint: spec.credential_fingerprint,
            })
        });
        let _ = tx.send(result);
    });
    rx
}

fn probe_endpoint(endpoint: &ImapProbeEndpoint) -> Result<core::ServerCapabilities, String> {
    probe_tls_capabilities_with_transport(
        &endpoint.endpoint,
        &endpoint.user,
        endpoint.password.as_str(),
        &endpoint.auth,
        &endpoint.tls,
        &endpoint.ca_bundle,
        &endpoint.certificate_pin_sha256,
    )
}

fn probe_auth_endpoint(endpoint: &ImapProbeEndpoint) -> Result<(), String> {
    probe_tls_authentication_with_transport(
        &endpoint.endpoint,
        &endpoint.user,
        endpoint.password.as_str(),
        &endpoint.auth,
        &endpoint.tls,
        &endpoint.ca_bundle,
        &endpoint.certificate_pin_sha256,
    )
}
