//! OpenID Connect sign-in for the control plane.
//!
//! Authorization-code flow with PKCE (S256), `state`, and `nonce`. The
//! pending sign-in is stored server-side by the digest of its `state` and
//! consumed once. The ID token is verified against the provider's JWKS
//! (see `jwt`). There is no automatic account creation: an identity maps to
//! an existing user only through an earlier link, or, when the provider is
//! configured for it, through a verified email address of an existing user.

use super::audit::AuditEntry;
use super::authn;
use super::jwt::{self, Expected, Jwks, JwtError};
use super::store::{Actor, Store, StoreError, StoreResult, append_audit, now};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use sha2::Digest as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub(super) const SCHEMA_V3: &str = "
CREATE TABLE oidc_identities(
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    linked_at INTEGER NOT NULL,
    PRIMARY KEY(issuer, subject)
) STRICT;
CREATE TABLE oidc_pending(
    state_hash BLOB PRIMARY KEY CHECK(length(state_hash) = 32),
    provider TEXT NOT NULL,
    nonce TEXT NOT NULL,
    verifier TEXT NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
";

const PENDING_SECONDS: i64 = 10 * 60;
const MAX_PENDING: i64 = 10_000;
const JWKS_TTL: Duration = Duration::from_secs(60 * 60);
const JWKS_REFRESH_FLOOR: Duration = Duration::from_secs(60);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// HTTP access to the identity provider, replaceable in tests.
pub(super) trait Fetch: Send + Sync {
    fn get_json(&self, url: &str) -> Result<serde_json::Value, String>;
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, String>;
}

/// HTTPS only, no redirects, bounded time and size.
pub(super) struct HttpsFetch {
    client: reqwest::blocking::Client,
}

impl HttpsFetch {
    pub(super) fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("could not build the OIDC HTTP client: {error}"))?;
        Ok(Self { client })
    }

    fn read(response: reqwest::blocking::Response) -> Result<serde_json::Value, String> {
        use std::io::Read as _;
        let status = response.status();
        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|error| format!("could not read the identity provider's response: {error}"))?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("the identity provider's response is too large".into());
        }
        if !status.is_success() {
            return Err(format!("the identity provider answered HTTP {status}"));
        }
        serde_json::from_slice(&body)
            .map_err(|_| "the identity provider's response is not JSON".to_owned())
    }
}

impl Fetch for HttpsFetch {
    fn get_json(&self, url: &str) -> Result<serde_json::Value, String> {
        let response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .map_err(|error| format!("could not reach {url}: {error}"))?;
        Self::read(response)
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
        let response = self
            .client
            .post(url)
            .header("Accept", "application/json")
            .form(form)
            .send()
            .map_err(|error| format!("could not reach {url}: {error}"))?;
        Self::read(response)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    public_url: String,
    #[serde(rename = "provider")]
    providers: Vec<ProviderFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderFile {
    name: String,
    issuer: String,
    client_id: String,
    #[serde(default)]
    client_secret_file: Option<PathBuf>,
    #[serde(default)]
    link_verified_email: bool,
    #[serde(default)]
    allowed_email_domains: Vec<String>,
}

pub(super) struct Provider {
    pub(super) name: String,
    issuer: String,
    client_id: String,
    client_secret: Option<crate::credentials::SecretString>,
    link_verified_email: bool,
    allowed_email_domains: Vec<String>,
    metadata: Mutex<Option<Metadata>>,
}

#[derive(Clone)]
struct Metadata {
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    jwks: Jwks,
    jwks_fetched: Option<Instant>,
}

pub(super) struct Oidc {
    public_url: String,
    pub(super) providers: Vec<Provider>,
    fetch: Box<dyn Fetch>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum OidcError {
    UnknownProvider,
    /// The sign-in cannot proceed (expired/replayed state, invalid token,
    /// no linked account). The message is safe to show the user.
    Rejected(String),
    /// The identity provider could not be reached or misbehaved.
    Upstream(String),
    Internal(String),
}

impl From<StoreError> for OidcError {
    fn from(error: StoreError) -> Self {
        OidcError::Internal(format!("{error:?}"))
    }
}

fn https_url(value: &str, what: &str) -> Result<String, String> {
    let url = url::Url::parse(value).map_err(|_| format!("{what} {value:?} is not a URL"))?;
    if url.scheme() != "https" || url.host_str().is_none() || url.fragment().is_some() {
        return Err(format!(
            "{what} {value:?} must be an https:// URL without a fragment"
        ));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

impl Oidc {
    /// Load the providers from an owner-only TOML file. Client secrets are
    /// read from their own owner-only files.
    pub(super) fn load(path: &Path) -> Result<Self, String> {
        let text = crate::credentials::read_secret_file(path)
            .map_err(|error| format!("OIDC configuration refused: {error}"))?;
        let file: ConfigFile = toml::from_str(text.as_str())
            .map_err(|error| format!("could not parse the OIDC configuration: {error}"))?;
        let public_url = https_url(&file.public_url, "public_url")?;
        if file.providers.is_empty() {
            return Err("the OIDC configuration names no [[provider]]".into());
        }
        let mut providers = Vec::new();
        for provider in file.providers {
            if provider.name.is_empty()
                || !provider
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || providers
                    .iter()
                    .any(|existing: &Provider| existing.name == provider.name)
            {
                return Err(format!(
                    "OIDC provider name {:?} must be unique letters, digits, and hyphens",
                    provider.name
                ));
            }
            let client_secret = provider
                .client_secret_file
                .as_deref()
                .map(crate::credentials::read_secret_file)
                .transpose()
                .map_err(|error| {
                    format!("OIDC client secret for {} refused: {error}", provider.name)
                })?;
            providers.push(Provider {
                issuer: https_url(&provider.issuer, "issuer")?,
                name: provider.name,
                client_id: provider.client_id,
                client_secret,
                link_verified_email: provider.link_verified_email,
                allowed_email_domains: provider
                    .allowed_email_domains
                    .into_iter()
                    .map(|domain| domain.trim().trim_start_matches('@').to_ascii_lowercase())
                    .collect(),
                metadata: Mutex::new(None),
            });
        }
        Ok(Self {
            public_url,
            providers,
            fetch: Box::new(HttpsFetch::new()?),
        })
    }

    fn provider(&self, name: &str) -> Result<&Provider, OidcError> {
        self.providers
            .iter()
            .find(|provider| provider.name == name)
            .ok_or(OidcError::UnknownProvider)
    }

    fn redirect_uri(&self, provider: &Provider) -> String {
        format!(
            "{}/v1/auth/oidc/{}/callback",
            self.public_url, provider.name
        )
    }

    /// Discovery metadata, fetched once and kept; the JWKS is refreshed
    /// hourly, or sooner (at most once a minute) when `force_jwks` asks.
    fn metadata(&self, provider: &Provider, force_jwks: bool) -> Result<Metadata, OidcError> {
        let mut cached = provider
            .metadata
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cached.is_none() {
            let document = self
                .fetch
                .get_json(&format!(
                    "{}/.well-known/openid-configuration",
                    provider.issuer
                ))
                .map_err(OidcError::Upstream)?;
            let field = |name: &str| {
                document[name]
                    .as_str()
                    .ok_or_else(|| OidcError::Upstream(format!("discovery document has no {name}")))
                    .and_then(|value| https_url(value, name).map_err(OidcError::Upstream))
            };
            if document["issuer"]
                .as_str()
                .map(|issuer| issuer.trim_end_matches('/'))
                != Some(provider.issuer.as_str())
            {
                return Err(OidcError::Upstream(
                    "the discovery document names a different issuer".into(),
                ));
            }
            *cached = Some(Metadata {
                authorization_endpoint: field("authorization_endpoint")?,
                token_endpoint: field("token_endpoint")?,
                jwks_uri: field("jwks_uri")?,
                jwks: Jwks::default(),
                jwks_fetched: None,
            });
        }
        let metadata = cached.as_mut().expect("metadata was just filled");
        let stale = metadata.jwks_fetched.is_none_or(|fetched| {
            let age = fetched.elapsed();
            age >= JWKS_TTL || (force_jwks && age >= JWKS_REFRESH_FLOOR)
        });
        if stale {
            let jwks = self
                .fetch
                .get_json(&metadata.jwks_uri)
                .map_err(OidcError::Upstream)?;
            metadata.jwks = serde_json::from_value(jwks)
                .map_err(|_| OidcError::Upstream("the provider's JWKS is not valid".into()))?;
            metadata.jwks_fetched = Some(Instant::now());
        }
        Ok(metadata.clone())
    }

    /// Begin a sign-in: remember the state, nonce, and PKCE verifier, and
    /// return the provider URL to send the browser to.
    pub(super) fn start(&self, store: &Store, provider: &str) -> Result<String, OidcError> {
        let provider = self.provider(provider)?;
        let metadata = self.metadata(provider, false)?;
        let state = random_text()?;
        let nonce = random_text()?;
        let verifier = random_text()?;
        let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
        store.save_pending(&state, &provider.name, &nonce, &verifier)?;
        let mut url = url::Url::parse(&metadata.authorization_endpoint)
            .map_err(|_| OidcError::Upstream("the authorization endpoint is not a URL".into()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &provider.client_id)
            .append_pair("redirect_uri", &self.redirect_uri(provider))
            .append_pair("scope", "openid email profile")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    /// Finish a sign-in from the provider's redirect and open a session.
    pub(super) fn callback(
        &self,
        store: &Store,
        provider_name: &str,
        code: &str,
        state: &str,
        request_id: &str,
    ) -> Result<(Zeroizing<String>, i64), OidcError> {
        let provider = self.provider(provider_name)?;
        let Some((pending_provider, nonce, verifier)) = store.take_pending(state)? else {
            return Err(OidcError::Rejected(
                "this sign-in expired or was already used; start again".into(),
            ));
        };
        if pending_provider != provider.name {
            return Err(OidcError::Rejected(
                "the sign-in state belongs to another provider".into(),
            ));
        }
        let metadata = self.metadata(provider, false)?;
        let redirect_uri = self.redirect_uri(provider);
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", provider.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ];
        if let Some(secret) = &provider.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        let tokens = self
            .fetch
            .post_form(&metadata.token_endpoint, &form)
            .map_err(OidcError::Upstream)?;
        let id_token = tokens["id_token"]
            .as_str()
            .ok_or_else(|| OidcError::Upstream("the token response has no id_token".into()))?;
        let expected = Expected {
            issuer: &provider.issuer,
            client_id: &provider.client_id,
            nonce: &nonce,
            now: now(),
        };
        let identity = match jwt::verify(id_token, &metadata.jwks, &expected) {
            Err(JwtError::UnknownKey) => {
                let refreshed = self.metadata(provider, true)?;
                jwt::verify(id_token, &refreshed.jwks, &expected)
            }
            other => other,
        }
        .map_err(|error| match error {
            JwtError::UnknownKey => {
                OidcError::Rejected("the ID token was signed by an unknown key".into())
            }
            JwtError::Invalid(message) => OidcError::Rejected(message),
        })?;
        let user = store.user_for_identity(provider, &identity, request_id)?;
        let Some(user) = user else {
            return Err(OidcError::Rejected(
                "no MailSwiftSync account is linked to this identity; ask an administrator to create your account".into(),
            ));
        };
        let actor = Actor {
            request_id,
            label: format!("user:{user}"),
        };
        store
            .open_session(&user, &format!("oidc:{}", provider.name), &actor)
            .map_err(OidcError::from)
    }
}

fn random_text() -> Result<String, OidcError> {
    authn::random_bytes::<32>()
        .map(|bytes| URL_SAFE_NO_PAD.encode(*bytes))
        .map_err(OidcError::Internal)
}

fn state_hash(state: &str) -> [u8; 32] {
    sha2::Sha256::digest(state.as_bytes()).into()
}

impl Store {
    fn save_pending(
        &self,
        state: &str,
        provider: &str,
        nonce: &str,
        verifier: &str,
    ) -> StoreResult<()> {
        let connection = self.lock();
        let at = now();
        connection.execute("DELETE FROM oidc_pending WHERE expires_at<=?1", [at])?;
        let pending: i64 =
            connection.query_row("SELECT COUNT(*) FROM oidc_pending", [], |row| row.get(0))?;
        if pending >= MAX_PENDING {
            return Err(StoreError::Conflict(
                "too many sign-ins are in progress; try again shortly".into(),
            ));
        }
        connection.execute(
            "INSERT INTO oidc_pending(state_hash,provider,nonce,verifier,expires_at) VALUES(?1,?2,?3,?4,?5)",
            params![state_hash(state).as_slice(), provider, nonce, verifier, at + PENDING_SECONDS],
        )?;
        Ok(())
    }

    /// Consume a pending sign-in; a state can be used once.
    fn take_pending(&self, state: &str) -> StoreResult<Option<(String, String, String)>> {
        let mut connection = self.lock();
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let hash = state_hash(state);
        let pending = tx
            .query_row(
                "SELECT provider,nonce,verifier FROM oidc_pending WHERE state_hash=?1 AND expires_at>?2",
                params![hash.as_slice(), now()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        tx.execute(
            "DELETE FROM oidc_pending WHERE state_hash=?1",
            [hash.as_slice()],
        )?;
        tx.commit()?;
        Ok(pending)
    }

    /// The enabled user an identity signs in as. An unlinked identity links
    /// to an existing user only by a verified email the provider allows.
    fn user_for_identity(
        &self,
        provider: &Provider,
        identity: &jwt::Identity,
        request_id: &str,
    ) -> Result<Option<String>, OidcError> {
        let mut connection = self.lock();
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let linked: Option<String> = tx
            .query_row(
                "SELECT i.user_id FROM oidc_identities i JOIN users u ON u.id=i.user_id WHERE i.issuer=?1 AND i.subject=?2 AND u.disabled_at IS NULL",
                params![provider.issuer, identity.subject],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        if linked.is_some() {
            return Ok(linked);
        }
        let already_linked: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM oidc_identities WHERE issuer=?1 AND subject=?2)",
                params![provider.issuer, identity.subject],
                |row| row.get(0),
            )
            .map_err(StoreError::from)?;
        let email = identity
            .email
            .as_deref()
            .filter(|_| !already_linked && provider.link_verified_email && identity.email_verified);
        let Some(email) = email else {
            return Ok(None);
        };
        let domain = email
            .rsplit_once('@')
            .map(|(_, domain)| domain.to_ascii_lowercase());
        if !provider.allowed_email_domains.is_empty()
            && domain.is_none_or(|domain| !provider.allowed_email_domains.contains(&domain))
        {
            return Ok(None);
        }
        // Link only a user with no identity from this issuer yet, so a
        // second account at the provider cannot take over the user.
        let user: Option<String> = tx
            .query_row(
                "SELECT u.id FROM users u WHERE u.email=?1 AND u.disabled_at IS NULL
                 AND NOT EXISTS(SELECT 1 FROM oidc_identities i WHERE i.user_id=u.id AND i.issuer=?2)",
                params![email, provider.issuer],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        let Some(user) = user else {
            return Ok(None);
        };
        let at = now();
        tx.execute(
            "INSERT INTO oidc_identities(issuer,subject,user_id,linked_at) VALUES(?1,?2,?3,?4)",
            params![provider.issuer, identity.subject, user, at],
        )
        .map_err(StoreError::from)?;
        append_audit(
            &tx,
            &AuditEntry {
                at,
                request_id,
                actor: &format!("user:{user}"),
                tenant: None,
                action: "identity.link",
                resource: &format!("user:{user}"),
                allowed: true,
                detail: &format!("oidc:{} verified email", provider.name),
            },
        )?;
        tx.commit().map_err(StoreError::from)?;
        Ok(Some(user))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::jwt::test_keys::Signer;
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    pub(in crate::cli::control) const ISSUER: &str = "https://idp.example";

    /// A scripted identity provider that signs whatever claims the test
    /// queues for the next token request.
    pub(in crate::cli::control) struct FakeIdp {
        pub(in crate::cli::control) signer: Signer,
        pub(in crate::cli::control) next_claims: Mutex<serde_json::Value>,
        pub(in crate::cli::control) token_requests: Mutex<Vec<Vec<(String, String)>>>,
    }

    impl Fetch for Arc<FakeIdp> {
        fn get_json(&self, url: &str) -> Result<serde_json::Value, String> {
            match url {
                "https://idp.example/.well-known/openid-configuration" => Ok(json!({
                    "issuer": ISSUER,
                    "authorization_endpoint": "https://idp.example/authorize",
                    "token_endpoint": "https://idp.example/token",
                    "jwks_uri": "https://idp.example/jwks",
                })),
                "https://idp.example/jwks" => {
                    Ok(serde_json::to_value(KeysOut(&self.signer.jwks())).unwrap())
                }
                _ => Err(format!("unexpected GET {url}")),
            }
        }

        fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
            assert_eq!(url, "https://idp.example/token");
            self.token_requests.lock().unwrap().push(
                form.iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            );
            let claims = self.next_claims.lock().unwrap().clone();
            Ok(
                json!({ "id_token": self.signer.sign(&json!({ "alg": "ES256", "kid": "ec-1" }), &claims) }),
            )
        }
    }

    struct KeysOut<'a>(&'a Jwks);

    impl serde::Serialize for KeysOut<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let keys = self
                .0
                .keys
                .iter()
                .map(|key| {
                    json!({
                        "kty": key.kty, "kid": key.kid, "use": key.usage, "alg": key.alg,
                        "n": key.n, "e": key.e, "crv": key.crv, "x": key.x, "y": key.y,
                    })
                })
                .collect::<Vec<_>>();
            json!({ "keys": keys }).serialize(serializer)
        }
    }

    pub(in crate::cli::control) fn oidc(idp: &Arc<FakeIdp>, link_verified_email: bool) -> Oidc {
        Oidc {
            public_url: "https://control.msp.example".into(),
            providers: vec![Provider {
                name: "idp".into(),
                issuer: ISSUER.into(),
                client_id: "client-1".into(),
                client_secret: None,
                link_verified_email,
                allowed_email_domains: vec!["msp.example".into()],
                metadata: Mutex::new(None),
            }],
            fetch: Box::new(Arc::clone(idp)),
        }
    }

    pub(in crate::cli::control) fn fake_idp() -> Arc<FakeIdp> {
        Arc::new(FakeIdp {
            signer: Signer::new(),
            next_claims: Mutex::new(json!({})),
            token_requests: Mutex::new(Vec::new()),
        })
    }

    /// Start a sign-in and return its state and nonce from the redirect URL.
    pub(in crate::cli::control) fn begin(oidc: &Oidc, store: &Store) -> (String, String, String) {
        let url = url::Url::parse(&oidc.start(store, "idp").unwrap()).unwrap();
        let query = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .unwrap()
        };
        assert_eq!(query("code_challenge_method"), "S256");
        assert_eq!(
            query("redirect_uri"),
            "https://control.msp.example/v1/auth/oidc/idp/callback"
        );
        (query("state"), query("nonce"), query("code_challenge"))
    }

    pub(in crate::cli::control) fn claims(
        nonce: &str,
        subject: &str,
        email: &str,
        verified: bool,
    ) -> serde_json::Value {
        json!({
            "iss": ISSUER, "sub": subject, "aud": "client-1", "exp": now() + 300, "iat": now(),
            "nonce": nonce, "email": email, "email_verified": verified,
        })
    }

    fn store_with_user(email: &str) -> (Store, String) {
        let store = Store::in_memory();
        let password = authn::test_secret();
        let admin = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let actor = Actor {
            request_id: "t",
            label: format!("user:{admin}"),
        };
        let user = store.create_user(email, "Eng", None, &actor).unwrap();
        (store, user)
    }

    #[test]
    fn a_verified_email_links_once_and_later_sign_ins_use_the_subject() {
        let idp = fake_idp();
        let oidc = oidc(&idp, true);
        let (store, user) = store_with_user("eng@msp.example");

        let (state, nonce, challenge) = begin(&oidc, &store);
        *idp.next_claims.lock().unwrap() = claims(&nonce, "sub-1", "eng@msp.example", true);
        let (session, _) = oidc.callback(&store, "idp", "code-1", &state, "r").unwrap();
        assert_eq!(store.principal(&session).unwrap().unwrap().user_id, user);
        let request = idp.token_requests.lock().unwrap()[0].clone();
        let verifier = &request
            .iter()
            .find(|(key, _)| key == "code_verifier")
            .unwrap()
            .1;
        assert_eq!(
            URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes())),
            challenge
        );

        assert!(
            matches!(
                oidc.callback(&store, "idp", "code-1", &state, "r"),
                Err(OidcError::Rejected(_))
            ),
            "a state is single-use"
        );

        // Same subject, changed email: still the linked user.
        let (state, nonce, _) = begin(&oidc, &store);
        *idp.next_claims.lock().unwrap() =
            claims(&nonce, "sub-1", "renamed@elsewhere.example", false);
        let (session, _) = oidc.callback(&store, "idp", "code-2", &state, "r").unwrap();
        assert_eq!(store.principal(&session).unwrap().unwrap().user_id, user);

        // A different subject with the same verified email cannot take over
        // a user who already has an identity from this issuer.
        let (state, nonce, _) = begin(&oidc, &store);
        *idp.next_claims.lock().unwrap() = claims(&nonce, "sub-2", "eng@msp.example", true);
        assert!(matches!(
            oidc.callback(&store, "idp", "code-3", &state, "r"),
            Err(OidcError::Rejected(_))
        ));
    }

    #[test]
    fn unverified_unknown_or_disallowed_identities_do_not_sign_in() {
        let idp = fake_idp();
        let (store, _) = store_with_user("eng@msp.example");
        for (link, email, verified) in [
            (true, "eng@msp.example", false),
            (true, "nobody@msp.example", true),
            (false, "eng@msp.example", true),
        ] {
            let oidc = oidc(&idp, link);
            let (state, nonce, _) = begin(&oidc, &store);
            *idp.next_claims.lock().unwrap() = claims(&nonce, "sub-x", email, verified);
            assert!(
                matches!(
                    oidc.callback(&store, "idp", "c", &state, "r"),
                    Err(OidcError::Rejected(_))
                ),
                "{email} verified={verified} link={link}"
            );
        }
        let oidc = oidc(&idp, true);
        let (state, nonce, _) = begin(&oidc, &store);
        *idp.next_claims.lock().unwrap() = claims(&nonce, "sub-x", "eng@other.example", true);
        assert!(matches!(
            oidc.callback(&store, "idp", "c", &state, "r"),
            Err(OidcError::Rejected(_))
        ));

        // A token for a different sign-in's nonce is refused.
        let (state, _, _) = begin(&oidc, &store);
        *idp.next_claims.lock().unwrap() =
            claims(&authn::test_secret(), "sub-1", "eng@msp.example", true);
        assert!(matches!(
            oidc.callback(&store, "idp", "c", &state, "r"),
            Err(OidcError::Rejected(_))
        ));
        assert_eq!(
            oidc.callback(&store, "missing", "c", "s", "r"),
            Err(OidcError::UnknownProvider)
        );
    }

    #[test]
    fn configuration_files_load_with_validated_providers() {
        let directory = std::env::temp_dir().join(format!("mss-oidc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let load = |text: &str| {
            let path = directory.join(format!("{}.toml", uuid::Uuid::new_v4()));
            crate::credentials::write_secret_file(&path, text).unwrap();
            Oidc::load(&path).map(|oidc| oidc.providers.len())
        };
        let provider = |name: &str, issuer: &str| {
            format!("[[provider]]\nname = \"{name}\"\nissuer = \"{issuer}\"\nclient_id = \"c\"\n")
        };
        let header = "public_url = \"https://control.msp.example\"\n";
        assert_eq!(
            load(&format!(
                "{header}{}{}",
                provider("entra", "https://login.example/t/v2.0"),
                provider("google", "https://accounts.google.com")
            )),
            Ok(2)
        );
        for bad in [
            format!(
                "{header}{}{}",
                provider("dup", "https://a.example"),
                provider("dup", "https://b.example")
            ),
            format!("{header}{}", provider("bad name", "https://a.example")),
            format!("{header}{}", provider("plain", "http://a.example")),
            "public_url = \"http://control.msp.example\"\n".to_owned()
                + &provider("x", "https://a.example"),
            header.to_owned(),
            format!(
                "{header}unexpected = 1\n{}",
                provider("x", "https://a.example")
            ),
        ] {
            assert!(load(&bad).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn configuration_requires_https_and_unique_names() {
        assert!(https_url("http://idp.example", "issuer").is_err());
        assert!(https_url("https://idp.example/#x", "issuer").is_err());
        assert_eq!(
            https_url("https://idp.example/", "issuer").unwrap(),
            "https://idp.example"
        );
    }
}
