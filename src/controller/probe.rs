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
    probe_tls_live_readiness_with_transport,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};

/// Account tests running at once across the process. Each holds a thread,
/// sockets, and credentials until its network deadline, and a superseded
/// request keeps running until then, so admission is capped rather than
/// left to how often the operator clicks.
const MAX_ACTIVE_PROBES: usize = 4;
static ACTIVE_PROBES: AtomicUsize = AtomicUsize::new(0);
const PROBES_BUSY: &str =
    "Too many account tests are already running; wait for one to finish and try again.";

/// One admitted probe; dropping it frees the slot.
struct ProbeSlot(&'static AtomicUsize);

impl ProbeSlot {
    fn acquire() -> Option<Self> {
        Self::acquire_in(&ACTIVE_PROBES)
    }

    fn acquire_in(active: &'static AtomicUsize) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |running| {
                (running < MAX_ACTIVE_PROBES).then_some(running + 1)
            })
            .ok()
            .map(|_| Self(active))
    }
}

impl Drop for ProbeSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

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
    spawn_capability_probe_in(ProbeSlot::acquire(), spec)
}

fn spawn_capability_probe_in(
    slot: Option<ProbeSlot>,
    spec: CapabilityProbeSpec,
) -> Receiver<CapabilityProbeResult> {
    let (tx, rx) = mpsc::channel();
    let Some(slot) = slot else {
        let _ = tx.send(CapabilityProbeResult {
            request_id: spec.request_id,
            plan_fingerprint: spec.plan_fingerprint,
            result: Err(PROBES_BUSY.into()),
        });
        return rx;
    };
    std::thread::spawn(move || {
        let _slot = slot;
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
    spawn_live_auth_probe_in(ProbeSlot::acquire(), spec)
}

fn spawn_live_auth_probe_in(
    slot: Option<ProbeSlot>,
    spec: LiveAuthProbeSpec,
) -> Receiver<Result<LiveAuthProof, String>> {
    let (tx, rx) = mpsc::channel();
    let Some(slot) = slot else {
        let _ = tx.send(Err(PROBES_BUSY.into()));
        return rx;
    };
    std::thread::spawn(move || {
        let _slot = slot;
        let result = probe_auth_endpoint(&spec.source).and_then(|_| {
            let endpoint = &spec.destination;
            probe_tls_live_readiness_with_transport(
                &endpoint.endpoint,
                &endpoint.user,
                endpoint.password.as_str(),
                &endpoint.auth,
                &endpoint.tls,
                &endpoint.ca_bundle,
                &endpoint.certificate_pin_sha256,
            )
            .map(|destination_quota| LiveAuthProof {
                plan_fingerprint: spec.plan_fingerprint,
                credential_fingerprint: spec.credential_fingerprint,
                destination_quota,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unreachable_endpoint() -> ImapProbeEndpoint {
        ImapProbeEndpoint {
            endpoint: "probe-cap.invalid".into(),
            user: "user@example.test".into(),
            password: "secret".into(),
            auth: "password".into(),
            tls: "imaps".into(),
            ca_bundle: String::new(),
            certificate_pin_sha256: String::new(),
        }
    }

    #[test]
    fn probes_beyond_the_cap_are_refused_without_a_thread() {
        // A private counter, so probes started by other tests cannot take
        // these slots.
        static ACTIVE: AtomicUsize = AtomicUsize::new(0);
        let held = (0..MAX_ACTIVE_PROBES)
            .map(|_| ProbeSlot::acquire_in(&ACTIVE).expect("a free slot"))
            .collect::<Vec<_>>();
        assert!(ProbeSlot::acquire_in(&ACTIVE).is_none());

        let result = spawn_capability_probe_in(
            ProbeSlot::acquire_in(&ACTIVE),
            CapabilityProbeSpec {
                request_id: "busy".into(),
                plan_fingerprint: "plan".into(),
                source: unreachable_endpoint(),
                destination: unreachable_endpoint(),
            },
        )
        .recv()
        .unwrap();
        assert_eq!(result.request_id, "busy");
        assert_eq!(result.result.unwrap_err(), PROBES_BUSY);
        let auth = spawn_live_auth_probe_in(
            ProbeSlot::acquire_in(&ACTIVE),
            LiveAuthProbeSpec {
                plan_fingerprint: "plan".into(),
                credential_fingerprint: "credential".into(),
                source: unreachable_endpoint(),
                destination: unreachable_endpoint(),
            },
        )
        .recv()
        .unwrap();
        assert_eq!(auth.err().as_deref(), Some(PROBES_BUSY));

        drop(held);
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0, "slots are released");
        let slot = ProbeSlot::acquire_in(&ACTIVE).expect("a slot frees up");
        drop(slot);
    }
}
