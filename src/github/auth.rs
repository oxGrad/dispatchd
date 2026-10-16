//! GitHub App authentication, minus the network: parsing the App's private
//! key and minting the short-lived App JWT. Signing goes through `ring` (already
//! in the tree via rustls) rather than a JWT crate - every current JWT
//! crate's RSA backend means either `aws-lc-rs` (a C toolchain in the musl
//! cross builds) or the `rsa` crate (an open RustSec advisory `rust-audit`
//! would fail on), and an RS256 JWT is three base64url segments.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};

/// GitHub rejects an App JWT whose `exp` is more than 10 minutes after
/// `iat`; 9 leaves room for clock skew on top of the backdated `iat`.
const JWT_LIFETIME_SECS: i64 = 9 * 60;
/// Backdates `iat`, as GitHub's docs recommend, so a host clock running
/// slightly ahead of GitHub's doesn't produce a "not yet valid" token.
const JWT_IAT_SKEW_SECS: i64 = 60;

/// A parsed App private key, ready to sign JWTs.
pub struct AppKey(RsaKeyPair);

impl AppKey {
    /// Parses the `.pem` GitHub hands out when you generate an App private
    /// key - PKCS#1 (`BEGIN RSA PRIVATE KEY`), which is what GitHub issues
    /// - or PKCS#8 (`BEGIN PRIVATE KEY`), in case it was converted.
    pub fn from_pem(pem: &str) -> Result<AppKey> {
        let (label, der) = decode_pem(pem)?;
        let pair = match label.as_str() {
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der)
                .map_err(|e| anyhow::anyhow!("not a valid RSA private key: {e}"))?,
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der)
                .map_err(|e| anyhow::anyhow!("not a valid RSA private key: {e}"))?,
            other => bail!(
                "expected a GitHub App private key (BEGIN RSA PRIVATE KEY), found BEGIN {other}"
            ),
        };
        Ok(AppKey(pair))
    }

    /// Mints the App JWT (RS256, `iss` = the App ID) used to call the
    /// `/app` endpoints and to exchange for an installation token. `now` is
    /// Unix seconds, passed in so this stays deterministic under test.
    pub fn app_jwt(&self, app_id: u64, now: i64) -> Result<String> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = serde_json::json!({
            "iat": now - JWT_IAT_SKEW_SECS,
            "exp": now + JWT_LIFETIME_SECS,
            "iss": app_id.to_string(),
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");

        let mut signature = vec![0; self.0.public().modulus_len()];
        self.0
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| anyhow::anyhow!("failed to sign the GitHub App JWT"))?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }
}

/// Splits a single PEM block into its label (`RSA PRIVATE KEY`) and the
/// decoded DER bytes. Tolerates surrounding whitespace and CRLF line
/// endings (a key copied through a Windows machine), nothing fancier.
fn decode_pem(pem: &str) -> Result<(String, Vec<u8>)> {
    let pem = pem.trim();
    let first = pem.lines().next().unwrap_or_default().trim();
    let label = first
        .strip_prefix("-----BEGIN ")
        .and_then(|rest| rest.strip_suffix("-----"))
        .context("not a PEM file (expected a -----BEGIN ...----- line)")?
        .to_string();
    let footer = format!("-----END {label}-----");
    let body: String = pem
        .lines()
        .skip(1)
        .map(str::trim)
        .take_while(|line| *line != footer)
        .collect();
    if !pem.lines().any(|line| line.trim() == footer) {
        bail!("PEM file is truncated (no {footer} line)");
    }
    let der = STANDARD
        .decode(body)
        .context("PEM body is not valid base64")?;
    Ok((label, der))
}

/// An installation access token. GitHub issues them for one hour; the
/// sync (design phase 2) caches and refreshes them ahead of `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationToken {
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    // Throwaway keys generated for these tests only - never registered with
    // any GitHub App.
    const TEST_KEY_PKCS1: &str = include_str!("testdata/test_app_key.pem");
    const TEST_KEY_PKCS8: &str = include_str!("testdata/test_app_key_pkcs8.pem");

    fn decode_segment(segment: &str) -> serde_json::Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).unwrap()).unwrap()
    }

    #[test]
    fn app_jwt_has_rs256_header_and_github_claims() {
        let key = AppKey::from_pem(TEST_KEY_PKCS1).unwrap();
        let jwt = key.app_jwt(123456, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        assert_eq!(
            decode_segment(parts[0]),
            serde_json::json!({"alg": "RS256", "typ": "JWT"})
        );
        assert_eq!(
            decode_segment(parts[1]),
            serde_json::json!({
                "iat": 1_700_000_000 - 60,
                "exp": 1_700_000_000 + 540,
                "iss": "123456",
            })
        );
    }

    #[test]
    fn app_jwt_signature_verifies_against_the_keys_public_half() {
        let key = AppKey::from_pem(TEST_KEY_PKCS1).unwrap();
        let jwt = key.app_jwt(1, 1_700_000_000).unwrap();
        let (signing_input, signature) = jwt.rsplit_once('.').unwrap();

        let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.0.public().as_ref());
        public
            .verify(
                signing_input.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature).unwrap(),
            )
            .expect("signature should verify");
    }

    #[test]
    fn from_pem_accepts_pkcs8_too() {
        let pkcs1 = AppKey::from_pem(TEST_KEY_PKCS1).unwrap();
        let pkcs8 = AppKey::from_pem(TEST_KEY_PKCS8).unwrap();
        // Same key, two encodings.
        assert_eq!(pkcs1.0.public().as_ref(), pkcs8.0.public().as_ref());
    }

    #[test]
    fn from_pem_tolerates_crlf_and_surrounding_whitespace() {
        let crlf = format!("\n  {}\n", TEST_KEY_PKCS1.replace('\n', "\r\n"));
        AppKey::from_pem(&crlf).unwrap();
    }

    #[test]
    fn from_pem_rejects_a_non_pem_file() {
        let err = AppKey::from_pem("ghp_notakey").err().unwrap();
        assert!(err.to_string().contains("not a PEM file"), "{err}");
    }

    #[test]
    fn from_pem_rejects_a_public_key_or_certificate() {
        let pem = "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n";
        let err = AppKey::from_pem(pem).err().unwrap();
        assert!(err.to_string().contains("BEGIN PUBLIC KEY"), "{err}");
    }

    #[test]
    fn from_pem_rejects_a_truncated_file() {
        let truncated: String = TEST_KEY_PKCS1
            .lines()
            .take(5)
            .collect::<Vec<_>>()
            .join("\n");
        let err = AppKey::from_pem(&truncated).err().unwrap();
        assert!(err.to_string().contains("truncated"), "{err}");
    }
}
