//! Blocking client for the DuoClip Worker (`worker/SPEC.md`).
//!
//! Every request except `POST /v1/devices` and `GET /v1/health` is signed with the device key
//! (see [`crate::signing`]). Errors never contain secrets, signatures or presigned URLs.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use duoclip_presence::{HeartbeatRequest, HeartbeatResponse};
use duoclip_proto::{ClipId, CrewId, DeviceId, Quality};
use serde::Deserialize;
use serde_json::{json, Value};
use ureq::http;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::Agent;

use crate::identity::{validate_crew_name, Identity};
use crate::signing::{
    next_timestamp, sign_request, HEADER_DEVICE, HEADER_SIGNATURE, HEADER_TIMESTAMP,
};
use crate::NetError;

/// Largest JSON response body accepted from the Worker.
const MAX_API_BODY: u64 = 1024 * 1024;
/// Largest presigned download accepted: the Worker's 64 MiB chunk limit plus headroom.
const MAX_DOWNLOAD_BYTES: u64 = 65 * 1024 * 1024;
/// Content type the presigned uploads are signed for.
const UPLOAD_CONTENT_TYPE: &str = "application/octet-stream";
const USER_AGENT: &str = concat!("duoclip-net/", env!("CARGO_PKG_VERSION"));

/// An invite code created by [`WorkerClient::create_invite`].
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Invite {
    /// 10 characters of `A-Z` / `2-9`; valid for 24 h and 5 uses.
    pub code: String,
    /// Unix ms when the code stops working.
    pub expires_at: u64,
}

impl std::fmt::Debug for Invite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The code is a join credential: keep it out of logs.
        f.debug_struct("Invite")
            .field("code", &"<oculto>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// One member of a group (`GET /v1/crews/:crew/members`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Member {
    /// The member's device id.
    pub device_id: DeviceId,
    /// The name the member chose.
    pub display_name: String,
}

/// Body of `POST /v1/clips/:clip/upload-urls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRequest {
    /// The uploader's own device id (the Worker rejects anything else).
    pub pov: DeviceId,
    /// Quality tier.
    pub quality: Quality,
    /// Chunk indices to presign (distinct).
    pub indices: Vec<u32>,
    /// Exact ciphertext size of each chunk, parallel to `indices`.
    pub sizes: Vec<u64>,
    /// Exact ciphertext size of the manifest when its URL is wanted too.
    pub manifest_size: Option<u64>,
}

/// Body of `POST /v1/clips/:clip/download-urls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Whose POV to download.
    pub pov: DeviceId,
    /// Quality tier.
    pub quality: Quality,
    /// Chunk indices to presign (distinct).
    pub indices: Vec<u32>,
    /// Also presign the manifest.
    pub manifest: bool,
}

/// A presigned chunk URL. `Debug` hides the URL.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct PresignedChunk {
    /// Chunk index.
    pub index: u32,
    /// Object key (no secrets).
    pub key: String,
    /// Presigned URL, valid for [`PresignedUrls::expires_in`] seconds.
    pub url: String,
    /// Uploads only: the exact `Content-Length` the PUT must send.
    #[serde(default)]
    pub content_length: Option<u64>,
}

/// A presigned manifest URL. `Debug` hides the URL.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct PresignedObject {
    /// Object key (no secrets).
    pub key: String,
    /// Presigned URL.
    pub url: String,
    /// Uploads only: the exact `Content-Length` the PUT must send.
    #[serde(default)]
    pub content_length: Option<u64>,
}

macro_rules! redacted_debug {
    ($ty:ident { $($field:ident),* }) => {
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty))
                    $(.field(stringify!($field), &self.$field))*
                    .field("url", &"<oculto>")
                    .finish()
            }
        }
    };
}
redacted_debug!(PresignedChunk {
    index,
    key,
    content_length
});
redacted_debug!(PresignedObject {
    key,
    content_length
});

/// Response of `upload-urls` and `download-urls`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PresignedUrls {
    /// Seconds the URLs stay valid.
    pub expires_in: u64,
    /// Unix ms when they stop working.
    pub expires_at: u64,
    /// Headers the upload must send (uploads only).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// One entry per requested chunk.
    pub chunks: Vec<PresignedChunk>,
    /// The manifest, when requested.
    #[serde(default)]
    pub manifest: Option<PresignedObject>,
}

/// Response of `DELETE /v1/clips/:clip`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeleteOutcome {
    /// How many stored objects were removed.
    pub deleted_objects: u64,
    /// False when some objects are left for the next sweep.
    pub complete: bool,
}

pub(crate) fn system_now_ms() -> u64 {
    // The Worker accepts +-5 minutes. This is not clip synchronization (that uses the DuoClip
    // global clock), so the system clock is the right thing here.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

/// Splits `scheme://authority` + tail and checks scheme / host / port rules shared by the base
/// URL and presigned URLs. Never echoes the input in errors (presigned URLs are secrets).
fn split_url<'a>(raw: &'a str, what: &str) -> Result<(&'a str, &'a str, &'a str), NetError> {
    let bad = || NetError::Config(format!("{what} inválido"));
    let (scheme, rest) = raw.split_once("://").ok_or_else(bad)?;
    if scheme != "https" && scheme != "http" {
        return Err(bad());
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace)
    {
        return Err(bad());
    }
    let (host, port) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(bad)?;
        let (host, after) = authority.split_at(close + 1);
        (host, after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty()
        || (!host.starts_with('[')
            && !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'))
    {
        return Err(bad());
    }
    if let Some(p) = port {
        let valid = !p.is_empty()
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n));
        if !valid {
            return Err(bad());
        }
    }
    if scheme == "http" && !is_loopback_host(host) {
        return Err(NetError::Config(format!(
            "{what} precisa usar https (http só é aceito para localhost / 127.0.0.1)"
        )));
    }
    Ok((scheme, authority, tail))
}

/// Validates a Worker base URL and returns it normalized as `scheme://host[:port]` without a
/// trailing slash. `https` is required except for `http://localhost`, `http://127.0.0.1` and
/// `http://[::1]`.
pub fn normalize_base_url(raw: &str) -> Result<String, NetError> {
    let raw = raw.trim();
    let (scheme, authority, tail) = split_url(raw, "o endereço do servidor")?;
    if !(tail.is_empty() || tail == "/") {
        return Err(NetError::Config(
            "o endereço do servidor não deve ter caminho, parâmetros ou fragmento (use só https://host)".into(),
        ));
    }
    Ok(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

fn validate_presigned_url(url: &str) -> Result<(), NetError> {
    split_url(url, "a URL de transferência").map(|_| ())
}

/// Short, fixed Portuguese description of a transport failure. Deliberately ignores the
/// error's own text, which can embed the request URL.
fn describe_transport_error(e: &ureq::Error) -> String {
    use ureq::Error as E;
    match e {
        E::Timeout(_) => "tempo esgotado esperando o servidor".into(),
        E::HostNotFound => "servidor não encontrado (confira o endereço e a internet)".into(),
        E::ConnectionFailed => "não consegui conectar ao servidor".into(),
        E::Io(io) => format!("erro de conexão ({})", io.kind()),
        E::Tls(_) | E::TlsRequired => "erro na conexão segura (TLS)".into(),
        #[allow(unreachable_patterns)]
        _ => "falha na conexão com o servidor".into(),
    }
}

/// Extracts the Worker's `{"error": "<code>"}` code from an error body, if well formed.
fn error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let code = value.get("error")?.as_str()?;
    let ok = !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    ok.then(|| code.to_owned())
}

struct RawResponse {
    status: u16,
    body: Vec<u8>,
}

/// Blocking Worker client; see the crate docs.
pub struct WorkerClient {
    base: String,
    identity: Identity,
    api_agent: Agent,
    transfer_agent: Agent,
    last_timestamp_ms: u64,
    clock: fn() -> u64,
}

impl std::fmt::Debug for WorkerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerClient")
            .field("base", &self.base)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

fn build_agent(global: Duration) -> Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    let config = Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(global))
        .user_agent(USER_AGENT)
        .build();
    Agent::new_with_config(config)
}

impl WorkerClient {
    /// Creates a client. `base_url` must be `https://...` (or `http://localhost` /
    /// `http://127.0.0.1` for a local Worker).
    pub fn new(base_url: &str, identity: Identity) -> Result<Self, NetError> {
        Ok(Self {
            base: normalize_base_url(base_url)?,
            identity,
            api_agent: build_agent(Duration::from_secs(30)),
            transfer_agent: build_agent(Duration::from_secs(15 * 60)),
            last_timestamp_ms: 0,
            clock: system_now_ms,
        })
    }

    /// Replaces the clock (unit tests only).
    #[doc(hidden)]
    pub fn with_clock(mut self, clock: fn() -> u64) -> Self {
        self.clock = clock;
        self
    }

    /// The identity this client signs with (its `registered` flag is updated by
    /// [`WorkerClient::register_device`]).
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Mutable access to the identity (for example to remember group names).
    pub fn identity_mut(&mut self) -> &mut Identity {
        &mut self.identity
    }

    /// Gives the identity back.
    pub fn into_identity(self) -> Identity {
        self.identity
    }

    /// The normalized base URL.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn next_timestamp(&mut self) -> u64 {
        let ts = next_timestamp(self.last_timestamp_ms, (self.clock)());
        self.last_timestamp_ms = ts;
        ts
    }

    fn execute(
        agent: &Agent,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        max_body: u64,
    ) -> Result<RawResponse, NetError> {
        let mut builder = http::Request::builder().method(method).uri(url);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let config_err =
            |_| NetError::Config("não consegui montar o pedido para o servidor".to_owned());
        let response = match body {
            Some(b) => agent.run(builder.body(b).map_err(config_err)?),
            None => agent.run(builder.body(()).map_err(config_err)?),
        }
        .map_err(|e| NetError::Network(describe_transport_error(&e)))?;
        let (parts, mut resp_body) = response.into_parts();
        let body = resp_body
            .with_config()
            .limit(max_body)
            .read_to_vec()
            .map_err(|e| NetError::Network(describe_transport_error(&e)))?;
        Ok(RawResponse {
            status: parts.status.as_u16(),
            body,
        })
    }

    fn check_status(raw: RawResponse) -> Result<RawResponse, NetError> {
        if (200..300).contains(&raw.status) {
            Ok(raw)
        } else {
            Err(NetError::Http {
                status: raw.status,
                code: error_code(&raw.body),
            })
        }
    }

    /// Sends one API request. `signed` adds the three authentication headers.
    fn api(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&str>,
        signed: bool,
    ) -> Result<RawResponse, NetError> {
        let url = format!("{}{path}", self.base);
        let body_bytes = body.map(str::as_bytes);
        let mut headers: Vec<(&str, String)> = vec![("accept", "application/json".to_owned())];
        if body.is_some() {
            headers.push(("content-type", "application/json".to_owned()));
        }
        if signed {
            let ts = self.next_timestamp();
            let h = sign_request(
                self.identity.signing_key(),
                self.identity.device_id(),
                method,
                path,
                ts,
                body_bytes.unwrap_or(&[]),
            );
            headers.push((HEADER_DEVICE, h.device));
            headers.push((HEADER_TIMESTAMP, h.timestamp));
            headers.push((HEADER_SIGNATURE, h.signature));
        }
        let borrowed: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let raw = Self::execute(
            &self.api_agent,
            method,
            &url,
            &borrowed,
            body_bytes,
            MAX_API_BODY,
        )?;
        Self::check_status(raw)
    }

    fn parse<T: for<'de> Deserialize<'de>>(raw: &RawResponse) -> Result<T, NetError> {
        serde_json::from_slice(&raw.body)
            .map_err(|_| NetError::InvalidResponse("o corpo não está no formato esperado".into()))
    }

    /// `GET /v1/health`.
    pub fn health(&self) -> Result<(), NetError> {
        let url = format!("{}/v1/health", self.base);
        let raw = Self::execute(
            &self.api_agent,
            "GET",
            &url,
            &[("accept", "application/json")],
            None,
            MAX_API_BODY,
        )?;
        let raw = Self::check_status(raw)?;
        let value: Value = Self::parse(&raw)?;
        if value.get("ok") == Some(&Value::Bool(true)) {
            Ok(())
        } else {
            Err(NetError::InvalidResponse(
                "o servidor não respondeu ok".into(),
            ))
        }
    }

    /// `POST /v1/devices`: registers this PC's public key. Repeating it with the same key is
    /// harmless. On success the identity's `registered` flag is set.
    pub fn register_device(&mut self) -> Result<(), NetError> {
        let body = json!({
            "device_id": self.identity.device_id().to_string(),
            "public_key_b64": self.identity.public_key_b64(),
            "display_name": self.identity.display_name(),
        })
        .to_string();
        self.api("POST", "/v1/devices", Some(&body), false)?;
        self.identity.registered = true;
        Ok(())
    }

    /// `POST /v1/crews`: creates a group; this PC is its first member.
    pub fn create_crew(&mut self, name: &str) -> Result<CrewId, NetError> {
        let name = validate_crew_name(name)?;
        let body = json!({ "name": name }).to_string();
        let raw = self.api("POST", "/v1/crews", Some(&body), true)?;
        #[derive(Deserialize)]
        struct Created {
            crew_id: CrewId,
        }
        Ok(Self::parse::<Created>(&raw)?.crew_id)
    }

    /// `POST /v1/crews/:crew/invites`: a code valid for 24 h and 5 uses.
    pub fn create_invite(&mut self, crew: CrewId) -> Result<Invite, NetError> {
        let raw = self.api(
            "POST",
            &format!("/v1/crews/{crew}/invites"),
            Some("{}"),
            true,
        )?;
        Self::parse(&raw)
    }

    /// `POST /v1/crews/join`: joins a group with a friend's invite code.
    pub fn join_crew(&mut self, code: &str) -> Result<CrewId, NetError> {
        let code = code.trim();
        if code.is_empty() || code.len() > 64 {
            return Err(NetError::Config("código de convite inválido".into()));
        }
        let body = json!({ "code": code }).to_string();
        let raw = self.api("POST", "/v1/crews/join", Some(&body), true)?;
        #[derive(Deserialize)]
        struct Joined {
            crew_id: CrewId,
        }
        Ok(Self::parse::<Joined>(&raw)?.crew_id)
    }

    /// `GET /v1/crews/:crew/members`.
    pub fn members(&mut self, crew: CrewId) -> Result<Vec<Member>, NetError> {
        let raw = self.api("GET", &format!("/v1/crews/{crew}/members"), None, true)?;
        Self::parse(&raw)
    }

    /// `POST /v1/presence`. A `409` is an [`NetError::Http`] that converts with
    /// `HeartbeatFailure::from(&err)` into `Stale409`.
    pub fn heartbeat(&mut self, req: &HeartbeatRequest) -> Result<HeartbeatResponse, NetError> {
        let body = req
            .to_json()
            .map_err(|e| NetError::Config(format!("anúncio de presença inválido: {e}")))?;
        let raw = self.api("POST", "/v1/presence", Some(&body), true)?;
        HeartbeatResponse::from_json(&raw.body)
            .map_err(|_| NetError::InvalidResponse("anúncio de presença ilegível".into()))
    }

    /// `POST /v1/clips`: registers a clip so its POVs can be uploaded. `ttl_s` is capped at
    /// 72 h by the Worker. Repeating the call for the same clip is harmless.
    pub fn register_clip(
        &mut self,
        clip: ClipId,
        crew: CrewId,
        ttl_s: u32,
    ) -> Result<(), NetError> {
        let body = json!({
            "clip_id": clip.to_string(),
            "crew_id": crew.to_string(),
            "ttl_s": ttl_s,
        })
        .to_string();
        self.api("POST", "/v1/clips", Some(&body), true)?;
        Ok(())
    }

    /// `POST /v1/clips/:clip/upload-urls`.
    pub fn upload_urls(
        &mut self,
        clip: ClipId,
        req: &UploadRequest,
    ) -> Result<PresignedUrls, NetError> {
        if req.indices.len() != req.sizes.len() {
            return Err(NetError::Config(
                "cada bloco precisa de um tamanho (indices e sizes com o mesmo tamanho)".into(),
            ));
        }
        let mut body = json!({
            "pov": req.pov.to_string(),
            "quality": req.quality.as_str(),
            "indices": req.indices,
            "sizes": req.sizes,
            "manifest": req.manifest_size.is_some(),
        });
        if let (Some(size), Some(map)) = (req.manifest_size, body.as_object_mut()) {
            map.insert("manifest_size".into(), json!(size));
        }
        let raw = self.api(
            "POST",
            &format!("/v1/clips/{clip}/upload-urls"),
            Some(&body.to_string()),
            true,
        )?;
        Self::parse(&raw)
    }

    /// `POST /v1/clips/:clip/download-urls`.
    pub fn download_urls(
        &mut self,
        clip: ClipId,
        req: &DownloadRequest,
    ) -> Result<PresignedUrls, NetError> {
        let body = json!({
            "pov": req.pov.to_string(),
            "quality": req.quality.as_str(),
            "indices": req.indices,
            "manifest": req.manifest,
        });
        let raw = self.api(
            "POST",
            &format!("/v1/clips/{clip}/download-urls"),
            Some(&body.to_string()),
            true,
        )?;
        Self::parse(&raw)
    }

    /// `DELETE /v1/clips/:clip`: removes the clip's stored objects.
    pub fn delete_clip(&mut self, clip: ClipId) -> Result<DeleteOutcome, NetError> {
        let raw = self.api("DELETE", &format!("/v1/clips/{clip}"), None, true)?;
        Self::parse(&raw)
    }

    /// Uploads `bytes` to a presigned URL with `Content-Type: application/octet-stream` and
    /// an exact `Content-Length`. The URL is never logged or put in an error.
    pub fn put_presigned(&self, url: &str, bytes: &[u8]) -> Result<(), NetError> {
        validate_presigned_url(url)?;
        let raw = Self::execute(
            &self.transfer_agent,
            "PUT",
            url,
            &[("content-type", UPLOAD_CONTENT_TYPE)],
            Some(bytes),
            MAX_API_BODY,
        )?;
        Self::check_status(raw).map(|_| ())
    }

    /// Downloads from a presigned URL (at most 65 MiB).
    pub fn get_presigned(&self, url: &str) -> Result<Vec<u8>, NetError> {
        validate_presigned_url(url)?;
        let raw = Self::execute(
            &self.transfer_agent,
            "GET",
            url,
            &[],
            None,
            MAX_DOWNLOAD_BYTES,
        )?;
        Self::check_status(raw).map(|r| r.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        assert_eq!(
            normalize_base_url("https://duoclip.exemplo.workers.dev").unwrap(),
            "https://duoclip.exemplo.workers.dev"
        );
        assert_eq!(
            normalize_base_url(" https://Exemplo.dev/ ").unwrap(),
            "https://exemplo.dev"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8787").unwrap(),
            "http://127.0.0.1:8787"
        );
        assert!(normalize_base_url("http://localhost:8787/").is_ok());
        assert!(normalize_base_url("http://[::1]:8787").is_ok());
        for bad in [
            "http://exemplo.dev",
            "ftp://exemplo.dev",
            "exemplo.dev",
            "https://",
            "https://user:pw@exemplo.dev",
            "https://exemplo.dev/v1",
            "https://exemplo.dev?x=1",
            "https://exemplo.dev#frag",
            "https://exemplo.dev:99999",
            "https://exemplo.dev:",
            "https://exem plo.dev",
            "http://127.0.0.1.exemplo.dev",
            "",
        ] {
            assert!(
                normalize_base_url(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn presigned_url_rules_never_echo_the_url() {
        assert!(validate_presigned_url(
            "https://acc.r2.cloudflarestorage.com/b/k?X-Amz-Signature=abc"
        )
        .is_ok());
        let err =
            validate_presigned_url("http://evil.example/k?X-Amz-Signature=SEGREDO").unwrap_err();
        assert!(!err.to_string().contains("SEGREDO"));
        assert!(!err.to_string().contains("evil"));
        assert!(validate_presigned_url("nonsense").is_err());
    }

    #[test]
    fn error_codes_are_extracted_strictly() {
        assert_eq!(
            error_code(br#"{"error":"not_a_member","message":"x"}"#).as_deref(),
            Some("not_a_member")
        );
        assert_eq!(error_code(b"<html>"), None);
        assert_eq!(error_code(br#"{"error":"Has Spaces"}"#), None);
        assert_eq!(error_code(br#"{"error":5}"#), None);
        assert_eq!(error_code(&[0xff, 0xfe]), None);
    }

    #[test]
    fn presigned_debug_hides_urls() {
        let c = PresignedChunk {
            index: 1,
            key: "k".into(),
            url: "https://x/SEGREDO".into(),
            content_length: Some(3),
        };
        assert!(!format!("{c:?}").contains("SEGREDO"));
        let o = PresignedObject {
            key: "k".into(),
            url: "https://x/SEGREDO".into(),
            content_length: None,
        };
        assert!(!format!("{o:?}").contains("SEGREDO"));
        let i = Invite {
            code: "ABCDEFGH23".into(),
            expires_at: 1,
        };
        assert!(!format!("{i:?}").contains("ABCDEFGH23"));
    }

    #[test]
    fn presigned_urls_parse_like_the_worker_response() {
        let json = r#"{
            "expires_in": 900, "expires_at": 1760000900000,
            "headers": {"Content-Type": "application/octet-stream"},
            "chunks": [{"index": 0, "key": "clips/a", "content_length": 10, "url": "https://x/y"}],
            "manifest": {"key": "clips/m", "url": "https://x/z", "content_length": 5}
        }"#;
        let parsed: PresignedUrls = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.chunks.len(), 1);
        assert_eq!(parsed.chunks[0].content_length, Some(10));
        assert!(parsed.manifest.is_some());
        let json = r#"{"expires_in": 900, "expires_at": 1, "chunks": [], "manifest": null}"#;
        let parsed: PresignedUrls = serde_json::from_str(json).unwrap();
        assert!(parsed.manifest.is_none() && parsed.headers.is_empty());
    }
}
