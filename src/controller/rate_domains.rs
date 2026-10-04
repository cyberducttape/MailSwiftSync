//! Hierarchical, observation-driven admission for batch engine attempts.
//!
//! Providers throttle at scopes MailSwiftSync cannot see in advance: per
//! mailbox, per OAuth principal or credential, per tenant, per provider, or
//! per controller egress address. Every job therefore runs inside a path of
//! rate domains,
//!
//! ```text
//! global → provider → tenant → credential → mailbox
//! ```
//!
//! with one provider/tenant/credential/mailbox chain per side (source and
//! destination). Each domain carries an AIMD concurrency limit and an
//! escalating cooldown. A capacity failure is attributed to the narrowest
//! domain first — the throttled mailbox on the side that reported it — and an
//! ancestor is penalized only after it observes capacity failures from at
//! least `ESCALATION_DISTINCT_CHILDREN` distinct children within
//! `ESCALATION_WINDOW`. One customer's throttled tenant therefore no longer
//! pauses an unrelated tenant that happens to share the endpoint pair, while
//! a provider that throttles broadly is still discovered from observation.
//! No undocumented provider quota is encoded.

use crate::controller::failure::{FailureClass, control_error_text};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Bound on remembered domains; idle unpenalized domains are pruned eagerly.
const MAX_RATE_DOMAINS: usize = 4_096;
/// Quiet period after a cooldown expires before a domain's escalation
/// history is forgotten. Throttling episodes closer together than this keep
/// doubling the backoff instead of starting over at the base delay.
pub(crate) const ESCALATION_RESET_AFTER: Duration = Duration::from_secs(300);
/// Window in which distinct throttled children implicate their parent.
pub(crate) const ESCALATION_WINDOW: Duration = Duration::from_secs(120);
/// Distinct throttled children that make a parent domain the observed scope.
pub(crate) const ESCALATION_DISTINCT_CHILDREN: usize = 2;
const MAX_COOLDOWN: Duration = Duration::from_secs(120);
const DEFAULT_CAPACITY_DELAY: Duration = Duration::from_secs(5);

pub(crate) use crate::imap_probe::MailSide as Side;

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Source => "source",
        Side::Destination => "destination",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum DomainLevel {
    Global,
    Provider,
    Tenant,
    Credential,
    Mailbox,
}

impl DomainLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Provider => "provider",
            Self::Tenant => "tenant",
            Self::Credential => "credential",
            Self::Mailbox => "mailbox",
        }
    }
}

/// One rate domain. `scope` is canonical and already includes its ancestors'
/// identity, so equal keys always denote the same domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DomainKey {
    side: Option<Side>,
    level: DomainLevel,
    scope: String,
    provider: &'static str,
}

impl DomainKey {
    /// Operator-facing name, e.g. `source tenant example.com @ imap.example.com:993`.
    pub(crate) fn label(&self) -> String {
        match self.side {
            None => "all providers (global)".to_owned(),
            Some(side) => format!("{} {} {}", side_label(side), self.level.label(), self.scope),
        }
    }

    pub(crate) fn level(&self) -> DomainLevel {
        self.level
    }
}

/// The ordered domains one job must be admitted through: global first, then
/// each side's chain from provider down to mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RateDomainPath {
    global: DomainKey,
    source: [DomainKey; 4],
    destination: [DomainKey; 4],
}

/// Identity of one side of a job, as needed to place it in the hierarchy.
pub(crate) struct SideIdentity<'a> {
    /// Canonical `host:port` endpoint.
    pub(crate) endpoint: &'a str,
    pub(crate) user: &'a str,
    /// Explicit provider tenant/account scope. When absent, the account's
    /// email domain is used as a conservative best-effort grouping key.
    pub(crate) tenant: &'a str,
    /// Authentication principal: an OAuth refresh credential, a keyring
    /// credential, or empty to fall back to the user.
    pub(crate) principal: &'a str,
}

fn side_chain(side: Side, identity: &SideIdentity<'_>) -> [DomainKey; 4] {
    let endpoint = identity.endpoint;
    let provider = crate::core::provider_intelligence::canonical_provider(endpoint);
    let user = identity.user.trim().to_lowercase();
    // Mail domain of the account; accounts without one share the endpoint's
    // unnamed tenant, which is the server operator's own scope.
    let tenant = if identity.tenant.trim().is_empty() {
        user.rsplit_once('@')
            .map(|(_, domain)| domain.trim_end_matches('.').to_owned())
            .unwrap_or_default()
    } else {
        identity.tenant.trim().to_ascii_lowercase()
    };
    let principal = match identity.principal.trim() {
        "" => user.clone(),
        principal => principal.to_owned(),
    };
    let key = |level, scope: String| DomainKey {
        side: Some(side),
        level,
        scope,
        provider,
    };
    let tenant_scope = if tenant.is_empty() {
        format!("(no domain) @ {endpoint}")
    } else {
        format!("{tenant} @ {endpoint}")
    };
    [
        key(DomainLevel::Provider, endpoint.to_owned()),
        key(DomainLevel::Tenant, tenant_scope.clone()),
        key(
            DomainLevel::Credential,
            format!("{principal} in {tenant_scope}"),
        ),
        key(DomainLevel::Mailbox, format!("{user} @ {endpoint}")),
    ]
}

impl RateDomainPath {
    pub(crate) fn new(source: &SideIdentity<'_>, destination: &SideIdentity<'_>) -> Self {
        Self {
            global: DomainKey {
                side: None,
                level: DomainLevel::Global,
                scope: String::new(),
                provider: "global",
            },
            source: side_chain(Side::Source, source),
            destination: side_chain(Side::Destination, destination),
        }
    }

    /// Queue-fairness identity: the source tenant, which is the customer
    /// whose mailboxes are being moved.
    pub(crate) fn fairness_key(&self) -> &str {
        &self.source[1].scope
    }

    pub(crate) fn all(&self) -> impl Iterator<Item = &DomainKey> {
        std::iter::once(&self.global)
            .chain(self.source.iter())
            .chain(self.destination.iter())
    }

    fn chain(&self, side: Side) -> &[DomainKey; 4] {
        match side {
            Side::Source => &self.source,
            Side::Destination => &self.destination,
        }
    }

    #[cfg(test)]
    pub(crate) fn domain(&self, side: Side, level: DomainLevel) -> &DomainKey {
        match level {
            DomainLevel::Global => &self.global,
            level => &self.chain(side)[level as usize - 1],
        }
    }
}

struct DomainState {
    /// AIMD concurrency limit; starts at the worker ceiling (unconstrained).
    limit: f64,
    in_flight: usize,
    blocked_until: Instant,
    last_capacity_failure: Option<Instant>,
    consecutive_capacity_failures: u8,
    /// Throttled children observed recently, newest time per child.
    child_failures: Vec<(DomainKey, Instant)>,
}

impl DomainState {
    fn new(ceiling: f64, now: Instant) -> Self {
        Self {
            limit: ceiling,
            in_flight: 0,
            blocked_until: now,
            last_capacity_failure: None,
            consecutive_capacity_failures: 0,
            child_failures: Vec::new(),
        }
    }

    fn admits(&self, now: Instant) -> bool {
        self.blocked_until <= now && (self.in_flight as f64) < self.limit.floor().max(1.0)
    }

    /// Nothing about this domain differs from a fresh one.
    fn forgettable(&self, ceiling: f64, now: Instant) -> bool {
        self.in_flight == 0
            && self.limit >= ceiling
            && self
                .last_capacity_failure
                .is_none_or(|_| self.blocked_until + ESCALATION_RESET_AFTER <= now)
            && self
                .child_failures
                .iter()
                .all(|(_, at)| *at + ESCALATION_WINDOW <= now)
    }
}

/// Which side a capacity failure came from, when the error says so.
/// imapsync names the source Host1 and the destination Host2; the fresh
/// authentication probe reports its side explicitly.
pub(crate) fn failure_sides(error: &str) -> Vec<Side> {
    let text = control_error_text(error);
    let host1 = text.contains("Host1");
    let host2 = text.contains("Host2");
    match (host1, host2) {
        (true, false) => vec![Side::Source],
        (false, true) => vec![Side::Destination],
        _ => vec![Side::Source, Side::Destination],
    }
}

pub(crate) struct RateDomainLimiter {
    ceiling: f64,
    provider_ceilings: crate::organization_policy::ProviderRateCeilings,
    state: Mutex<HashMap<DomainKey, DomainState>>,
}

/// Why a path cannot be admitted right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocked {
    /// A domain is cooling down until this instant.
    Until(Instant),
    /// A domain is at its concurrency limit; a running attempt must end first.
    Slot,
    /// The limiter's state is poisoned; nothing can be admitted safely.
    Unavailable,
}

/// A held admission. Dropping it releases the in-flight slot in every domain.
pub(crate) struct Admission {
    limiter: Arc<RateDomainLimiter>,
    path: RateDomainPath,
    admitted_at: Instant,
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.limiter.release(&self.path);
    }
}

impl RateDomainLimiter {
    /// `ceiling` is the batch worker count, the most any domain can admit.
    #[cfg(test)]
    pub(crate) fn new(ceiling: usize) -> Self {
        Self::new_with_provider_ceilings(
            ceiling,
            crate::organization_policy::ProviderRateCeilings::default(),
        )
    }

    pub(crate) fn new_with_provider_ceilings(
        ceiling: usize,
        provider_ceilings: crate::organization_policy::ProviderRateCeilings,
    ) -> Self {
        Self {
            ceiling: ceiling.max(1) as f64,
            provider_ceilings,
            state: Mutex::new(HashMap::new()),
        }
    }

    fn domain_ceiling(&self, key: &DomainKey) -> f64 {
        let configured = match key.level {
            DomainLevel::Provider => self.provider_ceilings.provider.get(key.provider),
            DomainLevel::Tenant => self.provider_ceilings.tenant.get(key.provider),
            DomainLevel::Credential => self.provider_ceilings.credential.get(key.provider),
            DomainLevel::Global | DomainLevel::Mailbox => None,
        };
        configured.map_or(self.ceiling, |limit| {
            self.ceiling.min((*limit).max(1) as f64)
        })
    }

    /// Hold a slot in every domain of `path` atomically if all of them are
    /// out of cooldown and below their concurrency limit. Never blocks: the
    /// scheduler parks a blocked task instead of tying up a worker.
    pub(crate) fn try_admit(self: &Arc<Self>, path: &RateDomainPath) -> Result<Admission, Blocked> {
        let mut state = self.state.lock().map_err(|_| Blocked::Unavailable)?;
        let now = Instant::now();
        let mut cooling_until = None::<Instant>;
        let mut slot_limited = false;
        for domain in path.all().filter_map(|key| state.get(key)) {
            if domain.blocked_until > now {
                cooling_until = Some(cooling_until.map_or(domain.blocked_until, |until| {
                    until.max(domain.blocked_until)
                }));
            } else if !domain.admits(now) {
                slot_limited = true;
            }
        }
        if let Some(until) = cooling_until {
            return Err(Blocked::Until(until));
        }
        if slot_limited {
            return Err(Blocked::Slot);
        }
        for key in path.all() {
            if let Some(domain) = self.entry(&mut state, key, now) {
                domain.in_flight += 1;
            }
        }
        Ok(Admission {
            limiter: Arc::clone(self),
            path: path.clone(),
            admitted_at: now,
        })
    }

    fn entry<'a>(
        &self,
        state: &'a mut HashMap<DomainKey, DomainState>,
        key: &DomainKey,
        now: Instant,
    ) -> Option<&'a mut DomainState> {
        if !state.contains_key(key) {
            if state.len() >= MAX_RATE_DOMAINS {
                state.retain(|key, domain| !domain.forgettable(self.domain_ceiling(key), now));
            }
            if state.len() >= MAX_RATE_DOMAINS {
                return None;
            }
            state.insert(key.clone(), DomainState::new(self.domain_ceiling(key), now));
        }
        state.get_mut(key)
    }

    fn release(&self, path: &RateDomainPath) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let now = Instant::now();
        for key in path.all() {
            if let Some(domain) = state.get_mut(key) {
                domain.in_flight = domain.in_flight.saturating_sub(1);
                if domain.forgettable(self.domain_ceiling(key), now) {
                    state.remove(key);
                }
            }
        }
    }

    /// Additive increase: a launch admitted after a domain's last capacity
    /// failure completed, so that domain may run one more job concurrently.
    /// A long transfer admitted before throttling proves nothing and must not
    /// erase escalation earned by later failures.
    pub(crate) fn observe_success(&self, admission: &Admission) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let now = Instant::now();
        for key in admission.path.all() {
            if let Some(domain) = state.get_mut(key)
                && domain
                    .last_capacity_failure
                    .is_none_or(|failed| admission.admitted_at >= failed)
            {
                domain.limit = (domain.limit + 1.0).min(self.domain_ceiling(key));
                if domain.blocked_until <= now {
                    domain.consecutive_capacity_failures = 0;
                }
            }
        }
    }

    /// Attribute a capacity failure. Returns each domain that was penalized
    /// with the end of its cooldown; non-capacity failures change nothing.
    pub(crate) fn observe_failure(
        &self,
        admission: &Admission,
        error: &str,
    ) -> Vec<(DomainKey, Instant)> {
        self.observe_failure_on(admission, error, failure_sides(error))
    }

    /// As `observe_failure`, for a failure whose side is already known.
    pub(crate) fn observe_failure_on(
        &self,
        admission: &Admission,
        error: &str,
        sides: Vec<Side>,
    ) -> Vec<(DomainKey, Instant)> {
        let Some(first_side) = sides.first().copied() else {
            return Vec::new();
        };
        let path = &admission.path;
        // If Host1/Host2 cannot attribute a failure, don't test it against
        // either endpoint's provider vocabulary. Those signatures can imply
        // capacity for the wrong side and incorrectly cool down both paths.
        let provider = if sides.len() > 1 {
            crate::core::provider_intelligence::UNATTRIBUTED_PROVIDER
        } else {
            path.chain(first_side)[0].provider
        };
        let is_capacity =
            crate::controller::failure::classify_failure_for_provider(provider, error)
                == FailureClass::Capacity;
        if !is_capacity {
            return Vec::new();
        }
        let text = control_error_text(error);
        let server_requested =
            crate::core::provider_intelligence::provider_signal_for_provider(provider, text)
                .and_then(|signal| signal.retry_after);
        let base = server_requested
            .or_else(|| {
                crate::core::provider_intelligence::ProviderErrorClassifier::classify(
                    provider, text,
                )
                .suggested_retry_delay()
            })
            .unwrap_or(DEFAULT_CAPACITY_DELAY);
        let floor = server_requested.unwrap_or_default();
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        let mut penalized = Vec::new();
        for side in sides {
            // global, provider, tenant, credential, mailbox — broadest first.
            let lineage: Vec<&DomainKey> = std::iter::once(&path.global)
                .chain(path.chain(side).iter())
                .collect();
            let mailbox = lineage.len() - 1;
            if self.penalize(
                &mut state,
                lineage[mailbox],
                (base, floor),
                admission.admitted_at,
                now,
            ) {
                penalized.push(lineage[mailbox].clone());
            }
            // Each ancestor learns which of its children was throttled and
            // becomes the observed scope once enough distinct children are.
            for index in (0..mailbox).rev() {
                // Global's children are provider pairs, so one failure that
                // names no side (attributed to both) is one child, not two.
                let child = if index == 0 {
                    DomainKey {
                        side: None,
                        level: DomainLevel::Provider,
                        scope: format!("{} → {}", path.source[0].scope, path.destination[0].scope),
                        provider: "global",
                    }
                } else {
                    lineage[index + 1].clone()
                };
                let Some(parent) = self.entry(&mut state, lineage[index], now) else {
                    continue;
                };
                parent
                    .child_failures
                    .retain(|(key, at)| *key != child && *at + ESCALATION_WINDOW > now);
                parent.child_failures.push((child, now));
                if parent.child_failures.len() >= ESCALATION_DISTINCT_CHILDREN
                    && self.penalize(
                        &mut state,
                        lineage[index],
                        (base, floor),
                        admission.admitted_at,
                        now,
                    )
                    && !penalized.contains(lineage[index])
                {
                    penalized.push(lineage[index].clone());
                }
            }
        }
        penalized
            .into_iter()
            .filter_map(|key| state.get(&key).map(|domain| (key, domain.blocked_until)))
            .collect()
    }

    /// Multiplicative decrease plus escalating cooldown. A failure from a
    /// launch admitted before the domain's previous decrease is the same
    /// congestion episode and does not halve the limit again.
    fn penalize(
        &self,
        state: &mut HashMap<DomainKey, DomainState>,
        key: &DomainKey,
        (base, floor): (Duration, Duration),
        admitted_at: Instant,
        now: Instant,
    ) -> bool {
        let Some(domain) = self.entry(state, key, now) else {
            return false;
        };
        if domain.blocked_until + ESCALATION_RESET_AFTER <= now {
            domain.consecutive_capacity_failures = 0;
        }
        if domain
            .last_capacity_failure
            .is_none_or(|previous| admitted_at >= previous)
        {
            domain.limit = (domain.limit / 2.0).max(1.0);
        }
        let multiplier = 1u32 << domain.consecutive_capacity_failures.min(5);
        // Escalation is capped, but never below a delay the server asked for.
        let cooldown = base.saturating_mul(multiplier).min(MAX_COOLDOWN).max(floor);
        domain.blocked_until = domain.blocked_until.max(now + cooldown);
        domain.consecutive_capacity_failures =
            domain.consecutive_capacity_failures.saturating_add(1);
        domain.last_capacity_failure = Some(now);
        true
    }

    /// Hold a domain closed until `until`, as an observed capacity failure
    /// would, without depending on provider retry-delay tables.
    #[cfg(all(test, unix))]
    pub(crate) fn hold_until(&self, key: &DomainKey, until: Instant) {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let domain = self.entry(&mut state, key, now).unwrap();
        domain.blocked_until = until;
        domain.last_capacity_failure = Some(now);
        domain.consecutive_capacity_failures = 1;
    }

    #[cfg(test)]
    fn snapshot(&self, key: &DomainKey) -> Option<(f64, usize, Duration, u8)> {
        let now = Instant::now();
        self.state.lock().unwrap().get(key).map(|domain| {
            (
                domain.limit,
                domain.in_flight,
                domain.blocked_until.saturating_duration_since(now),
                domain.consecutive_capacity_failures,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUSY: &str = "too many requests";

    fn path(source_user: &str, destination_user: &str) -> RateDomainPath {
        RateDomainPath::new(
            &SideIdentity {
                endpoint: "imap.gmail.com:993",
                user: source_user,
                tenant: "",
                principal: "",
            },
            &SideIdentity {
                endpoint: "outlook.office365.com:993",
                user: destination_user,
                tenant: "",
                principal: "",
            },
        )
    }

    #[test]
    fn configured_tenant_ceiling_is_independent_of_other_tenants() {
        let limiter = Arc::new(RateDomainLimiter::new_with_provider_ceilings(
            3,
            crate::organization_policy::ProviderRateCeilings {
                tenant: [("gmail".to_owned(), 1)].into_iter().collect(),
                ..Default::default()
            },
        ));
        let first = path("one@example.com", "dest@outlook.com");
        let same_tenant = path("two@example.com", "other@outlook.com");
        let other_tenant = path("one@another.example", "third@outlook.com");

        let held = limiter.try_admit(&first).unwrap();
        assert!(matches!(
            limiter.try_admit(&same_tenant),
            Err(Blocked::Slot)
        ));
        let independent = limiter.try_admit(&other_tenant).unwrap();
        drop(held);
        assert!(limiter.try_admit(&same_tenant).is_ok());
        drop(independent);
    }

    #[test]
    fn explicit_tenant_scope_groups_mailboxes_across_email_domains() {
        let tenant = |user: &str| {
            RateDomainPath::new(
                &SideIdentity {
                    endpoint: "imap.gmail.com:993",
                    user,
                    tenant: "workspace-tenant-guid",
                    principal: user,
                },
                &SideIdentity {
                    endpoint: "outlook.office365.com:993",
                    user,
                    tenant: "exchange-tenant-guid",
                    principal: user,
                },
            )
        };
        let first = tenant("alice@brand-a.example");
        let second = tenant("bob@brand-b.example");
        assert_eq!(
            first.domain(Side::Source, DomainLevel::Tenant),
            second.domain(Side::Source, DomainLevel::Tenant)
        );
        assert_eq!(
            first.domain(Side::Destination, DomainLevel::Tenant),
            second.domain(Side::Destination, DomainLevel::Tenant)
        );
        assert_ne!(
            first.domain(Side::Source, DomainLevel::Tenant),
            first.domain(Side::Destination, DomainLevel::Tenant)
        );
    }

    #[test]
    fn provider_and_credential_ceilings_apply_at_their_own_scope() {
        let provider_limiter = Arc::new(RateDomainLimiter::new_with_provider_ceilings(
            3,
            crate::organization_policy::ProviderRateCeilings {
                provider: [("gmail".to_owned(), 1)].into_iter().collect(),
                ..Default::default()
            },
        ));
        let first_tenant = path("one@example.com", "dest@outlook.com");
        let second_tenant = path("two@another.example", "dest@outlook.com");
        let held = provider_limiter.try_admit(&first_tenant).unwrap();
        assert!(matches!(
            provider_limiter.try_admit(&second_tenant),
            Err(Blocked::Slot)
        ));
        drop(held);

        let credential_limiter = Arc::new(RateDomainLimiter::new_with_provider_ceilings(
            3,
            crate::organization_policy::ProviderRateCeilings {
                credential: [("gmail".to_owned(), 1)].into_iter().collect(),
                ..Default::default()
            },
        ));
        let source = |user: &str, principal: &str| {
            RateDomainPath::new(
                &SideIdentity {
                    endpoint: "imap.gmail.com:993",
                    user,
                    tenant: "",
                    principal,
                },
                &SideIdentity {
                    endpoint: "outlook.office365.com:993",
                    user: "dest@outlook.com",
                    tenant: "",
                    principal: "",
                },
            )
        };
        let credential_a = source("one@example.com", "keyring:account-a");
        let same_credential = source("two@example.com", "keyring:account-a");
        let independent_credential = source("one@example.com", "keyring:account-b");
        let held = credential_limiter.try_admit(&credential_a).unwrap();
        assert!(matches!(
            credential_limiter.try_admit(&same_credential),
            Err(Blocked::Slot)
        ));
        let independent = credential_limiter
            .try_admit(&independent_credential)
            .unwrap();
        drop(held);
        assert!(credential_limiter.try_admit(&same_credential).is_ok());
        drop(independent);
    }

    #[test]
    fn provider_domains_retain_provider_identity_at_execution_boundary() {
        let path = path("one@example.com", "two@example.com");
        assert_eq!(
            path.domain(Side::Source, DomainLevel::Provider).provider,
            "gmail"
        );
        assert_eq!(
            path.domain(Side::Destination, DomainLevel::Provider)
                .provider,
            "microsoft365"
        );
        assert_ne!(
            path.domain(Side::Source, DomainLevel::Provider),
            path.domain(Side::Destination, DomainLevel::Provider)
        );
    }

    fn admit(limiter: &Arc<RateDomainLimiter>, path: &RateDomainPath) -> Admission {
        limiter.try_admit(path).unwrap()
    }

    fn try_admit(limiter: &Arc<RateDomainLimiter>, path: &RateDomainPath) -> Option<Admission> {
        limiter.try_admit(path).ok()
    }

    #[test]
    fn one_throttled_tenant_does_not_pause_another_on_the_same_endpoints() {
        let limiter = Arc::new(RateDomainLimiter::new(8));
        let customer_a = path("alice@a.example", "alice@a.example");
        let customer_b = path("bob@b.example", "bob@b.example");

        let a = admit(&limiter, &customer_a);
        let penalized = limiter.observe_failure(&a, BUSY);
        drop(a);

        // Only the throttled mailboxes (one per side, side unknown) cool down.
        assert!(
            penalized
                .iter()
                .all(|(key, _)| key.level == DomainLevel::Mailbox),
            "{penalized:?}"
        );
        assert!(try_admit(&limiter, &customer_a).is_none());
        assert!(try_admit(&limiter, &customer_b).is_some());
    }

    #[test]
    fn failure_side_is_attributed_from_engine_and_probe_text() {
        assert_eq!(
            failure_sides("Host1 failure: too many connections"),
            [Side::Source]
        );
        assert_eq!(failure_sides("Host2: NO [THROTTLED]"), [Side::Destination]);
        assert_eq!(
            failure_sides("server busy"),
            [Side::Source, Side::Destination]
        );

        let limiter = Arc::new(RateDomainLimiter::new(4));
        let job = path("alice@a.example", "alice@a.example");
        let unrelated_job = path(
            "different-source@example.com",
            "different-destination@example.com",
        );
        let admission = admit(&limiter, &unrelated_job);
        let penalized = limiter.observe_failure(&admission, "Host2: too many requests");
        assert_eq!(penalized.len(), 1);
        assert_eq!(penalized[0].0.side, Some(Side::Destination));

        // A Gmail-only capacity phrase without a Host1/Host2 marker must not
        // be attributed to either the source or destination rate domain.
        let admission = admit(&limiter, &job);
        assert!(
            limiter
                .observe_failure(&admission, "Too many simultaneous connections")
                .is_empty()
        );
    }

    #[test]
    fn distinct_throttled_children_escalate_to_the_observed_scope() {
        let limiter = Arc::new(RateDomainLimiter::new(8));
        let tenant_a1 = path("one@a.example", "one@a.example");
        let tenant_a2 = path("two@a.example", "two@a.example");
        let tenant_b = path("bob@b.example", "bob@b.example");
        let tenant_c = path("carol@c.example", "carol@c.example");

        // Two mailboxes (distinct credentials) of tenant A throttled: tenant A
        // is the observed scope; tenant B is unaffected.
        let first = admit(&limiter, &tenant_a1);
        let second = admit(&limiter, &tenant_a2);
        limiter.observe_failure(&first, "Host1 too many requests");
        let escalated = limiter.observe_failure(&second, "Host1 too many requests");
        let tenant_a = tenant_a1.domain(Side::Source, DomainLevel::Tenant);
        assert!(
            escalated.iter().any(|(key, _)| key == tenant_a),
            "{escalated:?}"
        );
        assert!(
            !escalated
                .iter()
                .any(|(key, _)| key.level == DomainLevel::Provider)
        );
        drop((first, second));
        assert!(try_admit(&limiter, &path("three@a.example", "three@a.example")).is_none());
        assert!(try_admit(&limiter, &tenant_b).is_some());

        // A second throttled tenant on the same provider implicates the
        // provider itself (for example a per-source-IP limit).
        let third = admit(&limiter, &tenant_b);
        let escalated = limiter.observe_failure(&third, "Host1 too many requests");
        let provider = tenant_b.domain(Side::Source, DomainLevel::Provider);
        assert!(
            escalated.iter().any(|(key, _)| key == provider),
            "{escalated:?}"
        );
        drop(third);
        assert!(try_admit(&limiter, &tenant_c).is_none());
    }

    #[test]
    fn global_escalates_only_for_distinct_provider_pairs() {
        let limiter = Arc::new(RateDomainLimiter::new(8));
        let job = path("alice@a.example", "alice@a.example");
        let global = job.domain(Side::Source, DomainLevel::Global).clone();
        let admission = admit(&limiter, &job);
        // Side unknown: both chains are charged, but it is one provider pair.
        let penalized = limiter.observe_failure(&admission, BUSY);
        assert!(
            !penalized.iter().any(|(key, _)| *key == global),
            "{penalized:?}"
        );

        let other = RateDomainPath::new(
            &SideIdentity {
                endpoint: "imap.other.example:993",
                user: "carol@c.example",
                tenant: "",
                principal: "",
            },
            &SideIdentity {
                endpoint: "outlook.office365.com:993",
                user: "carol@c.example",
                tenant: "",
                principal: "",
            },
        );
        let second = admit(&limiter, &other);
        let penalized = limiter.observe_failure(&second, BUSY);
        assert!(
            penalized.iter().any(|(key, _)| *key == global),
            "{penalized:?}"
        );
    }

    #[test]
    fn shared_oauth_principal_is_its_own_rate_domain() {
        let identity = |user| SideIdentity {
            endpoint: "imap.gmail.com:993",
            user,
            tenant: "",
            principal: "oauth:workspace-admin",
        };
        let destination = SideIdentity {
            endpoint: "dest.example:993",
            user: "x@d.example",
            tenant: "",
            principal: "",
        };
        let first = RateDomainPath::new(&identity("one@a.example"), &destination);
        let second = RateDomainPath::new(&identity("two@a.example"), &destination);
        assert_eq!(
            first.domain(Side::Source, DomainLevel::Credential),
            second.domain(Side::Source, DomainLevel::Credential)
        );
        assert_ne!(
            first.domain(Side::Source, DomainLevel::Mailbox),
            second.domain(Side::Source, DomainLevel::Mailbox)
        );
    }

    #[test]
    fn aimd_halves_on_capacity_and_grows_back_on_later_success() {
        let limiter = Arc::new(RateDomainLimiter::new(8));
        let job = path("alice@a.example", "alice@a.example");
        let mailbox = job.domain(Side::Source, DomainLevel::Mailbox).clone();

        let admission = admit(&limiter, &job);
        limiter.observe_failure(&admission, "Host1 too many requests");
        assert_eq!(limiter.snapshot(&mailbox).unwrap().0, 4.0);
        // A second failure from the same pre-decrease launch is the same
        // congestion episode: the cooldown escalates, the limit does not halve.
        limiter.observe_failure(&admission, "Host1 too many requests");
        let (limit, _, _, failures) = limiter.snapshot(&mailbox).unwrap();
        assert_eq!((limit, failures), (4.0, 2));
        // Success of a launch that predates the failure proves nothing.
        limiter.observe_success(&admission);
        assert_eq!(limiter.snapshot(&mailbox).unwrap().0, 4.0);
        drop(admission);

        limiter
            .state
            .lock()
            .unwrap()
            .get_mut(&mailbox)
            .unwrap()
            .blocked_until = Instant::now();
        let later = admit(&limiter, &job);
        limiter.observe_success(&later);
        assert_eq!(limiter.snapshot(&mailbox).unwrap().0, 5.0);
    }

    #[test]
    fn blocked_admission_reports_cooldown_end_or_slot_wait() {
        let limiter = Arc::new(RateDomainLimiter::new(4));
        let job = path("alice@a.example", "alice@a.example");
        let mailbox = job.domain(Side::Source, DomainLevel::Mailbox).clone();
        let admission = admit(&limiter, &job);
        limiter
            .state
            .lock()
            .unwrap()
            .get_mut(&mailbox)
            .unwrap()
            .limit = 1.0;
        assert!(matches!(limiter.try_admit(&job), Err(Blocked::Slot)));
        let penalized = limiter.observe_failure(&admission, "Host1 too many requests");
        let until = penalized[0].1;
        assert!(matches!(limiter.try_admit(&job), Err(Blocked::Until(at)) if at == until));
    }

    #[test]
    fn concurrency_limit_holds_slots_until_admissions_drop() {
        let limiter = Arc::new(RateDomainLimiter::new(4));
        let tenant_one = path("one@a.example", "one@d.example");
        let tenant_two = path("two@a.example", "two@d.example");
        let provider = tenant_one
            .domain(Side::Source, DomainLevel::Provider)
            .clone();
        let held = admit(&limiter, &tenant_one);
        {
            let mut state = limiter.state.lock().unwrap();
            state.get_mut(&provider).unwrap().limit = 1.0;
        }
        assert_eq!(limiter.snapshot(&provider).unwrap().1, 1);
        assert!(try_admit(&limiter, &tenant_two).is_none());
        drop(held);
        assert!(try_admit(&limiter, &tenant_two).is_some());
    }

    #[test]
    fn cooldowns_escalate_then_reset_after_quiet_period() {
        let limiter = Arc::new(RateDomainLimiter::new(4));
        let job = path("alice@a.example", "alice@a.example");
        let mailbox = job.domain(Side::Source, DomainLevel::Mailbox).clone();
        let admission = admit(&limiter, &job);
        limiter.observe_failure(&admission, "Host1 too many requests");
        let first = limiter.snapshot(&mailbox).unwrap().2;
        limiter
            .state
            .lock()
            .unwrap()
            .get_mut(&mailbox)
            .unwrap()
            .blocked_until = Instant::now();
        limiter.observe_failure(&admission, "Host1 too many requests");
        let second = limiter.snapshot(&mailbox).unwrap().2;
        assert!(
            second > first + first / 2,
            "{second:?} should double {first:?}"
        );

        limiter
            .state
            .lock()
            .unwrap()
            .get_mut(&mailbox)
            .unwrap()
            .blocked_until = Instant::now() - ESCALATION_RESET_AFTER - Duration::from_secs(1);
        limiter.observe_failure(&admission, "Host1 too many requests");
        assert_eq!(limiter.snapshot(&mailbox).unwrap().3, 1);
    }

    #[test]
    fn non_capacity_failures_and_cancellation_change_nothing() {
        let limiter = Arc::new(RateDomainLimiter::new(4));
        let job = path("alice@a.example", "alice@a.example");
        let admission = admit(&limiter, &job);
        assert!(
            limiter
                .observe_failure(&admission, "connection closed by remote host")
                .is_empty()
        );
        drop(admission);
        // Idle, unpenalized domains are not retained.
        assert!(limiter.state.lock().unwrap().is_empty());
    }

    fn mailbox_cooldown(error: &str) -> Option<Duration> {
        let limiter = Arc::new(RateDomainLimiter::new(4));
        let job = path("alice@a.example", "alice@a.example");
        let mailbox = job.domain(Side::Source, DomainLevel::Mailbox).clone();
        let admission = admit(&limiter, &job);
        limiter.observe_failure(&admission, error);
        limiter
            .snapshot(&mailbox)
            .map(|(_, _, remaining, _)| remaining)
    }

    #[test]
    fn mixed_disconnect_and_capacity_error_activates_long_cooldown() {
        let remaining = mailbox_cooldown("connection closed: too many connections").unwrap();
        assert!(remaining >= Duration::from_secs(29), "{remaining:?}");
        assert!(
            mailbox_cooldown("connection closed by remote host")
                .is_none_or(|remaining| remaining.is_zero())
        );
    }

    #[test]
    fn server_requested_backoff_sets_the_cooldown_floor() {
        let remaining = mailbox_cooldown(
            "Host2 BAD Request is throttled. Suggested Backoff Time: 299961 milliseconds",
        )
        .unwrap_or_default();
        // The source-side mailbox is untouched; check the destination side.
        let limiter = Arc::new(RateDomainLimiter::new(4));
        let job = path("alice@a.example", "alice@a.example");
        let admission = admit(&limiter, &job);
        let penalized = limiter.observe_failure(
            &admission,
            "Host2 BAD Request is throttled. Suggested Backoff Time: 299961 milliseconds",
        );
        let until = penalized[0].1;
        assert!(until.saturating_duration_since(Instant::now()) > Duration::from_secs(290));
        assert!(remaining.is_zero());
    }

    #[test]
    fn presentation_tail_cannot_change_cooldown_duration() {
        let remaining =
            mailbox_cooldown("too many connections; recent output: HTTP/1.1 429 Too Many Requests")
                .unwrap();
        assert!(remaining >= Duration::from_secs(29), "{remaining:?}");
        assert!(remaining < Duration::from_secs(60), "{remaining:?}");
    }
}
