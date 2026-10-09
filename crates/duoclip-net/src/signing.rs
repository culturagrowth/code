//! Request signing; must match `worker/src/auth.ts` byte for byte.
//!
//! Signed message: `"DC1\n" + METHOD + "\n" + PATH_WITH_QUERY + "\n" + TIMESTAMP + "\n" +
//! hex(sha256(body))`, signed with Ed25519 and sent as standard base64.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use duoclip_proto::DeviceId;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// First line of the canonical string (`SIGNATURE_VERSION` in the Worker).
pub const SIGNATURE_VERSION: &str = "DC1";
/// Header carrying the device id.
pub const HEADER_DEVICE: &str = "x-dc-device";
/// Header carrying the request timestamp (unix ms, decimal).
pub const HEADER_TIMESTAMP: &str = "x-dc-timestamp";
/// Header carrying the base64 Ed25519 signature.
pub const HEADER_SIGNATURE: &str = "x-dc-signature";

/// Lowercase hex SHA-256 of `body` (the empty body hashes the empty string).
pub fn body_sha256_hex(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

/// The exact string the Worker verifies the signature against. `method` is upper-cased like
/// the Worker does.
pub fn canonical_string(
    method: &str,
    path_with_query: &str,
    timestamp_ms: u64,
    body: &[u8],
) -> String {
    format!(
        "{SIGNATURE_VERSION}\n{}\n{path_with_query}\n{timestamp_ms}\n{}",
        method.to_ascii_uppercase(),
        body_sha256_hex(body)
    )
}

/// The three authentication headers of one request. `Debug` hides the signature.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedHeaders {
    /// `x-dc-device` value.
    pub device: String,
    /// `x-dc-timestamp` value.
    pub timestamp: String,
    /// `x-dc-signature` value (standard base64 of the 64-byte signature).
    pub signature: String,
}

impl std::fmt::Debug for SignedHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedHeaders")
            .field("device", &self.device)
            .field("timestamp", &self.timestamp)
            .field("signature", &"<oculto>")
            .finish()
    }
}

/// Signs one request.
pub fn sign_request(
    key: &SigningKey,
    device: DeviceId,
    method: &str,
    path_with_query: &str,
    timestamp_ms: u64,
    body: &[u8],
) -> SignedHeaders {
    let canonical = canonical_string(method, path_with_query, timestamp_ms, body);
    let signature = key.sign(canonical.as_bytes());
    SignedHeaders {
        device: device.to_string(),
        timestamp: timestamp_ms.to_string(),
        signature: STANDARD.encode(signature.to_bytes()),
    }
}

/// Next request timestamp: the clock reading, bumped so it is strictly greater than `last`.
/// Two quick requests therefore never share a signature (the Worker rejects replays).
pub fn next_timestamp(last: u64, now_ms: u64) -> u64 {
    now_ms.max(last.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;
    use serde_json::Value;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/worker_signing.json")).unwrap()
    }

    fn key_from_fixture(v: &Value) -> SigningKey {
        let seed = STANDARD.decode(v["seed_b64"].as_str().unwrap()).unwrap();
        SigningKey::from_bytes(&seed.try_into().unwrap())
    }

    #[test]
    fn matches_the_worker_typescript_fixture() {
        // The fixture was produced by the Worker's own `canonicalString` and WebCrypto Ed25519;
        // any format drift on either side breaks this test.
        let v = fixture();
        let key = key_from_fixture(&v);
        assert_eq!(
            STANDARD.encode(key.verifying_key().to_bytes()),
            v["public_key_b64"].as_str().unwrap()
        );
        for case in v["cases"].as_array().unwrap() {
            let ts: u64 = case["timestamp"].as_str().unwrap().parse().unwrap();
            let body = case["body"].as_str().unwrap().as_bytes();
            let method = case["method"].as_str().unwrap();
            let path = case["path"].as_str().unwrap();
            assert_eq!(
                body_sha256_hex(body),
                case["body_sha256_hex"].as_str().unwrap()
            );
            assert_eq!(
                canonical_string(method, path, ts, body),
                case["canonical"].as_str().unwrap()
            );
            let headers = sign_request(&key, DeviceId::new_random(), method, path, ts, body);
            assert_eq!(headers.signature, case["signature_b64"].as_str().unwrap());
            assert_eq!(headers.timestamp, case["timestamp"].as_str().unwrap());
        }
    }

    #[test]
    fn canonical_string_layout() {
        let s = canonical_string("get", "/v1/x?a=1", 42, b"");
        assert_eq!(
            s,
            "DC1\nGET\n/v1/x?a=1\n42\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn signature_verifies_with_the_public_key() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let device = DeviceId::new_random();
        let h = sign_request(&key, device, "POST", "/v1/crews", 1_000, b"{}");
        let raw = STANDARD.decode(&h.signature).unwrap();
        assert_eq!(raw.len(), 64);
        let sig = ed25519_dalek::Signature::from_slice(&raw).unwrap();
        let canonical = canonical_string("POST", "/v1/crews", 1_000, b"{}");
        assert!(key
            .verifying_key()
            .verify(canonical.as_bytes(), &sig)
            .is_ok());
        // A different body must not verify.
        let other = canonical_string("POST", "/v1/crews", 1_000, b"{ }");
        assert!(key.verifying_key().verify(other.as_bytes(), &sig).is_err());
    }

    #[test]
    fn timestamps_are_strictly_increasing() {
        assert_eq!(next_timestamp(0, 100), 100);
        assert_eq!(next_timestamp(100, 100), 101);
        assert_eq!(next_timestamp(100, 50), 101);
        assert_eq!(next_timestamp(100, 500), 500);
        assert_eq!(next_timestamp(u64::MAX, 5), u64::MAX);
    }

    #[test]
    fn debug_hides_the_signature() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let h = sign_request(&key, DeviceId::new_random(), "GET", "/v1/x", 1, b"");
        assert!(!format!("{h:?}").contains(&h.signature));
    }
}
