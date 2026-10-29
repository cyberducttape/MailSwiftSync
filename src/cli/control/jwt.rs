//! OpenID Connect ID-token verification with `ring`.
//!
//! Only asymmetric algorithms an identity provider publishes keys for are
//! accepted (RS256 and ES256). `none` and the HMAC algorithms are refused,
//! so a token cannot be "signed" with the client secret or not at all. The
//! key is chosen by `kid` from the provider's JWKS and must match the
//! token's algorithm. Claims are checked against the configured issuer and
//! client, the nonce of this login, and the clock with a small skew.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

const CLOCK_SKEW_SECONDS: i64 = 60;
const MAX_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct Jwk {
    pub(super) kty: String,
    #[serde(default)]
    pub(super) kid: Option<String>,
    #[serde(default, rename = "use")]
    pub(super) usage: Option<String>,
    #[serde(default)]
    pub(super) alg: Option<String>,
    #[serde(default)]
    pub(super) n: Option<String>,
    #[serde(default)]
    pub(super) e: Option<String>,
    #[serde(default)]
    pub(super) crv: Option<String>,
    #[serde(default)]
    pub(super) x: Option<String>,
    #[serde(default)]
    pub(super) y: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub(super) struct Jwks {
    pub(super) keys: Vec<Jwk>,
}

/// The verified identity an ID token asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Identity {
    pub(super) subject: String,
    pub(super) email: Option<String>,
    pub(super) email_verified: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum JwtError {
    /// No key in the JWKS has this `kid`; the caller may refresh the JWKS once.
    UnknownKey,
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> JwtError {
    JwtError::Invalid(message.into())
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    crit: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: i64,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<EmailVerified>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

/// Some providers send `email_verified` as a string.
#[derive(Deserialize)]
#[serde(untagged)]
enum EmailVerified {
    Bool(bool),
    Text(String),
}

pub(super) struct Expected<'a> {
    pub(super) issuer: &'a str,
    pub(super) client_id: &'a str,
    pub(super) nonce: &'a str,
    pub(super) now: i64,
}

fn decode(part: &str, what: &str) -> Result<Vec<u8>, JwtError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| invalid(format!("the ID token {what} is not base64url")))
}

pub(super) fn verify(
    token: &str,
    jwks: &Jwks,
    expected: &Expected<'_>,
) -> Result<Identity, JwtError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(invalid("the ID token is too large"));
    }
    let mut parts = token.split('.');
    let (Some(header_part), Some(payload_part), Some(signature_part), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid("the ID token is not a compact JWS"));
    };
    let header: Header = serde_json::from_slice(&decode(header_part, "header")?)
        .map_err(|_| invalid("the ID token header is not valid JSON"))?;
    if header.crit.is_some() {
        return Err(invalid("the ID token uses critical header extensions"));
    }
    // Refuse `none`, HMAC, and anything else before looking for a key, so
    // an unsupported algorithm never triggers a JWKS refresh.
    if !matches!(header.alg.as_str(), "RS256" | "ES256") {
        return Err(invalid(format!(
            "the ID token algorithm {:?} is not accepted; use RS256 or ES256",
            header.alg
        )));
    }
    let signature = decode(signature_part, "signature")?;
    let signed = format!("{header_part}.{payload_part}");
    let key = select_key(jwks, &header)?;
    verify_signature(&header.alg, key, signed.as_bytes(), &signature)?;

    let claims: Claims = serde_json::from_slice(&decode(payload_part, "payload")?)
        .map_err(|_| invalid("the ID token claims are not valid"))?;
    if claims.iss != expected.issuer {
        return Err(invalid("the ID token was issued by a different issuer"));
    }
    let audiences = match &claims.aud {
        Audience::One(audience) => vec![audience.as_str()],
        Audience::Many(audiences) => audiences.iter().map(String::as_str).collect(),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(invalid("the ID token is not meant for this client"));
    }
    if (audiences.len() > 1 || claims.azp.is_some())
        && claims.azp.as_deref() != Some(expected.client_id)
    {
        return Err(invalid(
            "the ID token's authorized party is not this client",
        ));
    }
    if claims.exp <= expected.now - CLOCK_SKEW_SECONDS {
        return Err(invalid("the ID token has expired"));
    }
    if claims
        .iat
        .is_some_and(|iat| iat > expected.now + CLOCK_SKEW_SECONDS)
        || claims
            .nbf
            .is_some_and(|nbf| nbf > expected.now + CLOCK_SKEW_SECONDS)
    {
        return Err(invalid("the ID token is not valid yet"));
    }
    let nonce_matches = claims.nonce.as_deref().is_some_and(|nonce| {
        crate::oauth_redirect::constant_time_eq(nonce.as_bytes(), expected.nonce.as_bytes())
    });
    if !nonce_matches {
        return Err(invalid("the ID token nonce does not match this sign-in"));
    }
    if claims.sub.is_empty() || claims.sub.len() > 255 {
        return Err(invalid("the ID token subject is not valid"));
    }
    Ok(Identity {
        subject: claims.sub,
        email: claims.email,
        email_verified: matches!(claims.email_verified, Some(EmailVerified::Bool(true)))
            || matches!(&claims.email_verified, Some(EmailVerified::Text(text)) if text == "true"),
    })
}

fn select_key<'a>(jwks: &'a Jwks, header: &Header) -> Result<&'a Jwk, JwtError> {
    let usable = |key: &&Jwk| {
        key.usage.as_deref().is_none_or(|usage| usage == "sig")
            && key.alg.as_deref().is_none_or(|alg| alg == header.alg)
    };
    match &header.kid {
        Some(kid) => jwks
            .keys
            .iter()
            .filter(usable)
            .find(|key| key.kid.as_deref() == Some(kid.as_str()))
            .ok_or(JwtError::UnknownKey),
        // Without a kid, accept only an unambiguous single signing key.
        None => {
            let mut candidates = jwks.keys.iter().filter(usable);
            match (candidates.next(), candidates.next()) {
                (Some(key), None) => Ok(key),
                _ => Err(invalid(
                    "the ID token names no key and the provider publishes several",
                )),
            }
        }
    }
}

fn verify_signature(
    alg: &str,
    key: &Jwk,
    message: &[u8],
    signature: &[u8],
) -> Result<(), JwtError> {
    let component = |value: &Option<String>, name: &str| {
        value
            .as_deref()
            .ok_or_else(|| invalid(format!("the signing key has no {name}")))
            .and_then(|value| decode(value, "key"))
    };
    let verified = match (alg, key.kty.as_str()) {
        ("RS256", "RSA") => ring::signature::RsaPublicKeyComponents {
            n: component(&key.n, "modulus")?,
            e: component(&key.e, "exponent")?,
        }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            message,
            signature,
        ),
        ("ES256", "EC") if key.crv.as_deref() == Some("P-256") => {
            let x = component(&key.x, "x coordinate")?;
            let y = component(&key.y, "y coordinate")?;
            if x.len() != 32 || y.len() != 32 {
                return Err(invalid("the P-256 signing key is malformed"));
            }
            let mut point = Vec::with_capacity(65);
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            ring::signature::UnparsedPublicKey::new(
                &ring::signature::ECDSA_P256_SHA256_FIXED,
                point,
            )
            .verify(message, signature)
        }
        ("RS256" | "ES256", _) => {
            return Err(invalid(
                "the signing key type does not match the token algorithm",
            ));
        }
        _ => {
            return Err(invalid(format!(
                "the ID token algorithm {alg:?} is not accepted; use RS256 or ES256"
            )));
        }
    };
    verified.map_err(|_| invalid("the ID token signature is not valid"))
}

#[cfg(test)]
pub(super) mod test_keys {
    //! Real keys for signing test tokens.
    use super::*;
    use ring::signature::KeyPair as _;

    pub(in crate::cli::control) struct Signer {
        ec: ring::signature::EcdsaKeyPair,
        rsa: ring::signature::RsaKeyPair,
    }

    impl Signer {
        pub(in crate::cli::control) fn new() -> Self {
            let random = ring::rand::SystemRandom::new();
            let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(
                &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                &random,
            )
            .unwrap();
            let ec = ring::signature::EcdsaKeyPair::from_pkcs8(
                &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                pkcs8.as_ref(),
                &random,
            )
            .unwrap();
            let rsa = ring::signature::RsaKeyPair::from_pkcs8(include_bytes!(
                "../../../tests/fixtures/control/oidc-test-rsa.pk8"
            ))
            .unwrap();
            Self { ec, rsa }
        }

        pub(in crate::cli::control) fn jwks(&self) -> Jwks {
            let point = self.ec.public_key().as_ref();
            let rsa: ring::rsa::PublicKeyComponents<Vec<u8>> = self.rsa.public().into();
            Jwks {
                keys: vec![
                    Jwk {
                        kty: "EC".into(),
                        kid: Some("ec-1".into()),
                        usage: Some("sig".into()),
                        alg: Some("ES256".into()),
                        crv: Some("P-256".into()),
                        x: Some(URL_SAFE_NO_PAD.encode(&point[1..33])),
                        y: Some(URL_SAFE_NO_PAD.encode(&point[33..65])),
                        n: None,
                        e: None,
                    },
                    Jwk {
                        kty: "RSA".into(),
                        kid: Some("rsa-1".into()),
                        usage: Some("sig".into()),
                        alg: None,
                        n: Some(URL_SAFE_NO_PAD.encode(&rsa.n)),
                        e: Some(URL_SAFE_NO_PAD.encode(&rsa.e)),
                        crv: None,
                        x: None,
                        y: None,
                    },
                ],
            }
        }

        /// A token with `header` and `claims`, signed by the key `kid`
        /// names (`ec-1` or `rsa-1`).
        pub(in crate::cli::control) fn sign(
            &self,
            header: &serde_json::Value,
            claims: &serde_json::Value,
        ) -> String {
            let signed = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(header.to_string()),
                URL_SAFE_NO_PAD.encode(claims.to_string())
            );
            let random = ring::rand::SystemRandom::new();
            let signature = if header["kid"] == "rsa-1" {
                let mut signature = vec![0; self.rsa.public().modulus_len()];
                self.rsa
                    .sign(
                        &ring::signature::RSA_PKCS1_SHA256,
                        &random,
                        signed.as_bytes(),
                        &mut signature,
                    )
                    .unwrap();
                signature
            } else {
                self.ec
                    .sign(&random, signed.as_bytes())
                    .unwrap()
                    .as_ref()
                    .to_vec()
            };
            format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_keys::Signer;
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;

    fn claims() -> serde_json::Value {
        json!({
            "iss": "https://idp.example",
            "sub": "subject-1",
            "aud": "client-1",
            "exp": NOW + 300,
            "iat": NOW,
            "nonce": "nonce-1",
            "email": "eng@msp.example",
            "email_verified": true,
        })
    }

    fn expected() -> Expected<'static> {
        Expected {
            issuer: "https://idp.example",
            client_id: "client-1",
            nonce: "nonce-1",
            now: NOW,
        }
    }

    #[test]
    fn rs256_and_es256_tokens_from_published_keys_verify() {
        let signer = Signer::new();
        for kid in ["ec-1", "rsa-1"] {
            let alg = if kid == "ec-1" { "ES256" } else { "RS256" };
            let token = signer.sign(&json!({ "alg": alg, "kid": kid }), &claims());
            let identity = verify(&token, &signer.jwks(), &expected()).unwrap();
            assert_eq!(identity.subject, "subject-1");
            assert!(identity.email_verified);
        }
    }

    #[test]
    fn forged_or_mismatched_tokens_are_refused() {
        let signer = Signer::new();
        let jwks = signer.jwks();
        let header = json!({ "alg": "ES256", "kid": "ec-1" });
        let refused = |token: String| verify(&token, &jwks, &expected()).unwrap_err();

        // Unsigned and HMAC tokens.
        let unsigned = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(json!({ "alg": "none", "kid": "ec-1" }).to_string()),
            URL_SAFE_NO_PAD.encode(claims().to_string())
        );
        assert!(matches!(refused(unsigned), JwtError::Invalid(_)));
        let mut hmac = signer.sign(&header, &claims());
        hmac.replace_range(
            ..hmac.find('.').unwrap(),
            &URL_SAFE_NO_PAD.encode(json!({ "alg": "HS256", "kid": "ec-1" }).to_string()),
        );
        assert!(matches!(refused(hmac), JwtError::Invalid(_)));

        // A tampered payload under a valid signature.
        let token = signer.sign(&header, &claims());
        let parts = token.split('.').collect::<Vec<_>>();
        let mut tampered = claims();
        tampered["sub"] = json!("admin");
        let forged = format!(
            "{}.{}.{}",
            parts[0],
            URL_SAFE_NO_PAD.encode(tampered.to_string()),
            parts[2]
        );
        assert!(matches!(refused(forged), JwtError::Invalid(_)));

        // Algorithm/key confusion: the EC key declares ES256, so an RS256
        // header naming it finds no usable key; the RSA key declares no alg,
        // so an ES256 header naming it is refused by key type.
        let confused = signer.sign(&json!({ "alg": "RS256", "kid": "ec-1" }), &claims());
        assert_eq!(refused(confused), JwtError::UnknownKey);
        let confused = signer.sign(&json!({ "alg": "ES256", "kid": "rsa-1" }), &claims());
        assert!(
            matches!(refused(confused), JwtError::Invalid(message) if message.contains("key type"))
        );

        assert_eq!(
            refused(signer.sign(&json!({ "alg": "ES256", "kid": "rotated" }), &claims())),
            JwtError::UnknownKey
        );

        for (field, value) in [
            ("iss", json!("https://evil.example")),
            ("aud", json!("other-client")),
            ("aud", json!(["client-1", "other-client"])),
            ("exp", json!(NOW - 120)),
            ("iat", json!(NOW + 600)),
            ("nbf", json!(NOW + 600)),
            ("nonce", json!("replayed-nonce")),
            ("sub", json!("")),
        ] {
            let mut altered = claims();
            altered[field] = value;
            assert!(
                matches!(
                    refused(signer.sign(&header, &altered)),
                    JwtError::Invalid(_)
                ),
                "{field}"
            );
        }
        let mut no_nonce = claims();
        no_nonce.as_object_mut().unwrap().remove("nonce");
        assert!(matches!(
            refused(signer.sign(&header, &no_nonce)),
            JwtError::Invalid(_)
        ));

        let mut with_azp = claims();
        with_azp["aud"] = json!(["client-1", "other-client"]);
        with_azp["azp"] = json!("client-1");
        assert!(verify(&signer.sign(&header, &with_azp), &jwks, &expected()).is_ok());

        let mut unverified = claims();
        unverified["email_verified"] = json!("false");
        assert!(
            !verify(&signer.sign(&header, &unverified), &jwks, &expected())
                .unwrap()
                .email_verified
        );
    }
}
