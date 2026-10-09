//! `WorkerClient` against a tiny in-process HTTP server on 127.0.0.1: checks the exact requests
//! the client sends (headers, signature, bodies) and how it reads success and error responses.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use duoclip_net::signing::{canonical_string, HEADER_DEVICE, HEADER_SIGNATURE, HEADER_TIMESTAMP};
use duoclip_net::{DownloadRequest, Identity, NetError, UploadRequest, WorkerClient};
use duoclip_presence::{HeartbeatFailure, HeartbeatRequest};
use duoclip_proto::{ClipId, CrewId, Quality};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Answers `replies.len()` requests in order, then stops. Returns the base URL and a handle
/// that yields the captured requests.
fn serve(replies: Vec<(u16, Vec<u8>)>) -> (String, JoinHandle<Vec<Captured>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let mut captured = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            captured.push(read_request(&mut stream));
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            let _ = stream.flush();
        }
        captured
    });
    ready_rx.recv().unwrap();
    (url, handle)
}

fn read_request(stream: &mut TcpStream) -> Captured {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "client closed before sending a full request");
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8(data[..header_end].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap().split(' ');
    let method = first.next().unwrap().to_owned();
    let path = first.next().unwrap().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let want: usize = headers
        .get("content-length")
        .map_or(0, |v| v.parse().unwrap());
    let mut body = data[header_end..].to_vec();
    while body.len() < want {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "client closed mid-body");
        body.extend_from_slice(&buf[..n]);
    }
    Captured {
        method,
        path,
        headers,
        body,
    }
}

fn json(value: serde_json::Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

fn client(url: &str) -> WorkerClient {
    let identity = Identity::generate("Nico").unwrap();
    WorkerClient::new(url, identity)
        .unwrap()
        .with_clock(|| 5_000)
}

/// Checks the three auth headers against the identity's public key, like the Worker does.
fn assert_signed(req: &Captured, identity: &Identity) {
    assert_eq!(req.headers[HEADER_DEVICE], identity.device_id().to_string());
    let ts: u64 = req.headers[HEADER_TIMESTAMP].parse().unwrap();
    let raw_sig = STANDARD.decode(&req.headers[HEADER_SIGNATURE]).unwrap();
    let sig = Signature::from_slice(&raw_sig).unwrap();
    let pk: [u8; 32] = STANDARD
        .decode(identity.public_key_b64())
        .unwrap()
        .try_into()
        .unwrap();
    let canonical = canonical_string(&req.method, &req.path, ts, &req.body);
    VerifyingKey::from_bytes(&pk)
        .unwrap()
        .verify(canonical.as_bytes(), &sig)
        .expect("the Worker would reject this signature");
}

#[test]
fn health_and_unsigned_registration() {
    let (url, server) = serve(vec![
        (200, json(serde_json::json!({"ok": true}))),
        (201, json(serde_json::json!({"device_id": "x"}))),
    ]);
    let mut c = client(&url);
    c.health().unwrap();
    assert!(!c.identity().registered);
    c.register_device().unwrap();
    assert!(c.identity().registered);

    let seen = server.join().unwrap();
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "/v1/health")
    );
    assert!(!seen[0].headers.contains_key(HEADER_SIGNATURE));
    assert_eq!(
        (seen[1].method.as_str(), seen[1].path.as_str()),
        ("POST", "/v1/devices")
    );
    assert!(!seen[1].headers.contains_key(HEADER_SIGNATURE));
    assert_eq!(seen[1].headers["content-type"], "application/json");
    let body: serde_json::Value = serde_json::from_slice(&seen[1].body).unwrap();
    assert_eq!(body["device_id"], c.identity().device_id().to_string());
    assert_eq!(body["public_key_b64"], c.identity().public_key_b64());
    assert_eq!(body["display_name"], "Nico");
}

#[test]
fn signed_requests_verify_and_timestamps_increase() {
    let crew = CrewId::new_random();
    let (url, server) = serve(vec![
        (201, json(serde_json::json!({"crew_id": crew.to_string()}))),
        (
            201,
            json(serde_json::json!({"code": "ABCDEFGH23", "expires_at": 1_760_000_000_000u64})),
        ),
        (
            200,
            json(
                serde_json::json!([{"device_id": "11111111-2222-4333-8444-555555555555", "display_name": "Ana"}]),
            ),
        ),
        (200, json(serde_json::json!({"crew_id": crew.to_string()}))),
    ]);
    let mut c = client(&url);
    assert_eq!(c.create_crew("  Amigos do LoL ").unwrap(), crew);
    let invite = c.create_invite(crew).unwrap();
    assert_eq!(invite.code, "ABCDEFGH23");
    let members = c.members(crew).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].display_name, "Ana");
    assert_eq!(c.join_crew(" abcdefgh23 ").unwrap(), crew);

    let seen = server.join().unwrap();
    let identity = c.identity();
    for req in &seen {
        assert_signed(req, identity);
    }
    // Same clock reading every time: the client must still never repeat a timestamp.
    let stamps: Vec<u64> = seen
        .iter()
        .map(|r| r.headers[HEADER_TIMESTAMP].parse().unwrap())
        .collect();
    assert_eq!(stamps, vec![5_000, 5_001, 5_002, 5_003]);

    assert_eq!(seen[0].path, "/v1/crews");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&seen[0].body).unwrap(),
        serde_json::json!({"name": "Amigos do LoL"})
    );
    assert_eq!(seen[1].path, format!("/v1/crews/{crew}/invites"));
    assert_eq!(
        (seen[2].method.as_str(), seen[2].path.as_str()),
        ("GET", format!("/v1/crews/{crew}/members").as_str())
    );
    assert!(seen[2].body.is_empty());
    assert_eq!(seen[3].path, "/v1/crews/join");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&seen[3].body).unwrap(),
        serde_json::json!({"code": "abcdefgh23"})
    );
}

#[test]
fn worker_errors_become_typed_portuguese_errors() {
    let (url, server) = serve(vec![
        (
            404,
            json(serde_json::json!({"error": "invalid_invite", "message": "x"})),
        ),
        (
            409,
            json(serde_json::json!({"error": "stale_presence", "message": "x"})),
        ),
        (502, b"<html>bad gateway</html>".to_vec()),
    ]);
    let mut c = client(&url);
    let err = c.join_crew("ABCDEFGH23").unwrap_err();
    assert_eq!(
        err,
        NetError::Http {
            status: 404,
            code: Some("invalid_invite".into())
        }
    );
    assert!(err.to_string().contains("convite inválido"));

    let hb = HeartbeatRequest {
        game: None,
        active_crew: None,
        seated_since_ms: None,
        seq: 1,
        online_since_ms: 10,
    };
    let err = c.heartbeat(&hb).unwrap_err();
    assert_eq!(HeartbeatFailure::from(&err), HeartbeatFailure::Stale409);

    let err = c.members(CrewId::new_random()).unwrap_err();
    assert_eq!(
        err,
        NetError::Http {
            status: 502,
            code: None
        }
    );
    assert_eq!(HeartbeatFailure::from(&err), HeartbeatFailure::Http(502));
    server.join().unwrap();
}

#[test]
fn malformed_success_bodies_are_invalid_responses() {
    let (url, server) = serve(vec![
        (201, b"not json".to_vec()),
        (200, json(serde_json::json!({"ok": true}))),
        (
            200,
            json(serde_json::json!({"crew_id": "NOT-A-LOWERCASE-UUID"})),
        ),
    ]);
    let mut c = client(&url);
    assert!(matches!(
        c.create_crew("x").unwrap_err(),
        NetError::InvalidResponse(_)
    ));
    let hb = HeartbeatRequest {
        game: None,
        active_crew: None,
        seated_since_ms: None,
        seq: 1,
        online_since_ms: 10,
    };
    let err = c.heartbeat(&hb).unwrap_err();
    assert!(matches!(err, NetError::InvalidResponse(_)));
    assert_eq!(
        HeartbeatFailure::from(&err),
        HeartbeatFailure::InvalidResponse
    );
    assert!(matches!(
        c.join_crew("ABCDEFGH23").unwrap_err(),
        NetError::InvalidResponse(_)
    ));
    server.join().unwrap();
}

#[test]
fn heartbeat_parses_the_worker_snapshot() {
    let crew = CrewId::new_random();
    let reply = serde_json::json!({
        "ok": true, "seen_at_ms": 100, "expires_at": 90_100, "heartbeat_interval_ms": 30_000,
        "crews": [{"crew_id": crew.to_string(), "members": [{
            "device_id": "11111111-2222-4333-8444-555555555555", "display_name": "Ana",
            "game": null, "active_crew": null, "seated_since_ms": null,
            "seq": 1, "online_since_ms": 50, "seen_at_ms": 100, "expires_at": 90_100
        }]}]
    });
    let (url, server) = serve(vec![(200, json(reply))]);
    let mut c = client(&url);
    let hb = HeartbeatRequest {
        game: Some("cs2".into()),
        active_crew: Some(crew.to_string()),
        seated_since_ms: Some(7),
        seq: 2,
        online_since_ms: 50,
    };
    let resp = c.heartbeat(&hb).unwrap();
    assert_eq!(resp.heartbeat_interval_ms, 30_000);
    assert_eq!(resp.crews.len(), 1);
    assert_eq!(resp.crews[0].members[0].display_name, "Ana");
    let seen = server.join().unwrap();
    assert_signed(&seen[0], c.identity());
    assert_eq!(seen[0].path, "/v1/presence");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&seen[0].body).unwrap(),
        serde_json::json!({
            "game": "cs2", "active_crew": crew.to_string(),
            "seated_since_ms": 7, "seq": 2, "online_since_ms": 50
        })
    );
}

#[test]
fn clip_routes_send_the_shapes_the_worker_validates() {
    let clip = ClipId::new_random();
    let crew = CrewId::new_random();
    let upload_reply = serde_json::json!({
        "expires_in": 900, "expires_at": 1,
        "headers": {"Content-Type": "application/octet-stream"},
        "chunks": [{"index": 0, "key": "k0", "url": "https://r2.example/k0?sig=1", "content_length": 10}],
        "manifest": {"key": "m", "url": "https://r2.example/m?sig=2", "content_length": 4}
    });
    let download_reply = serde_json::json!({
        "expires_in": 900, "expires_at": 1,
        "chunks": [{"index": 3, "key": "k3", "url": "https://r2.example/k3?sig=3"}],
        "manifest": null
    });
    let (url, server) = serve(vec![
        (
            201,
            json(serde_json::json!({"clip_id": clip.to_string(), "expires_at": 9})),
        ),
        (200, json(upload_reply)),
        (200, json(download_reply)),
        (
            200,
            json(serde_json::json!({"ok": true, "deleted_objects": 2, "complete": true})),
        ),
    ]);
    let mut c = client(&url);
    let pov = c.identity().device_id();
    c.register_clip(clip, crew, 3600).unwrap();
    let up = c
        .upload_urls(
            clip,
            &UploadRequest {
                pov,
                quality: Quality::Full,
                indices: vec![0],
                sizes: vec![10],
                manifest_size: Some(4),
            },
        )
        .unwrap();
    assert_eq!(up.chunks[0].content_length, Some(10));
    assert_eq!(up.manifest.as_ref().unwrap().content_length, Some(4));
    let down = c
        .download_urls(
            clip,
            &DownloadRequest {
                pov,
                quality: Quality::Proxy,
                indices: vec![3],
                manifest: false,
            },
        )
        .unwrap();
    assert!(down.manifest.is_none());
    let del = c.delete_clip(clip).unwrap();
    assert!(del.complete && del.deleted_objects == 2);
    // The presigned URLs never show up in Debug output.
    assert!(!format!("{up:?}").contains("sig=1"));

    let seen = server.join().unwrap();
    for req in &seen {
        assert_signed(req, c.identity());
    }
    let body = |i: usize| serde_json::from_slice::<serde_json::Value>(&seen[i].body).unwrap();
    assert_eq!(
        body(0),
        serde_json::json!({"clip_id": clip.to_string(), "crew_id": crew.to_string(), "ttl_s": 3600})
    );
    assert_eq!(seen[1].path, format!("/v1/clips/{clip}/upload-urls"));
    assert_eq!(
        body(1),
        serde_json::json!({
            "pov": pov.to_string(), "quality": "full", "indices": [0], "sizes": [10],
            "manifest": true, "manifest_size": 4
        })
    );
    assert_eq!(seen[2].path, format!("/v1/clips/{clip}/download-urls"));
    assert_eq!(
        body(2),
        serde_json::json!({"pov": pov.to_string(), "quality": "proxy", "indices": [3], "manifest": false})
    );
    assert_eq!(
        (seen[3].method.as_str(), seen[3].path.as_str()),
        ("DELETE", format!("/v1/clips/{clip}").as_str())
    );
}

#[test]
fn presigned_put_and_get_use_exact_length_and_hide_urls() {
    let (url, server) = serve(vec![
        (200, Vec::new()),
        (200, b"cipher-bytes".to_vec()),
        (
            403,
            b"<Error><Code>SignatureDoesNotMatch</Code></Error>".to_vec(),
        ),
    ]);
    let c = client(&url);
    let payload = vec![7u8; 1234];
    c.put_presigned(
        &format!("{url}/bucket/key?X-Amz-Signature=SEGREDO1"),
        &payload,
    )
    .unwrap();
    assert_eq!(
        c.get_presigned(&format!("{url}/bucket/key?X-Amz-Signature=SEGREDO2"))
            .unwrap(),
        b"cipher-bytes"
    );
    let err = c
        .get_presigned(&format!("{url}/bucket/key?X-Amz-Signature=SEGREDO3"))
        .unwrap_err();
    assert_eq!(
        err,
        NetError::Http {
            status: 403,
            code: None
        }
    );
    assert!(!err.to_string().contains("SEGREDO"));

    let seen = server.join().unwrap();
    assert_eq!(seen[0].method, "PUT");
    assert_eq!(seen[0].headers["content-type"], "application/octet-stream");
    assert_eq!(seen[0].headers["content-length"], "1234");
    assert_eq!(seen[0].body, payload);
    // Presigned requests are authorized by the URL alone.
    assert!(!seen[0].headers.contains_key(HEADER_SIGNATURE));
    assert!(!seen[1].headers.contains_key(HEADER_DEVICE));
}

#[test]
fn presigned_urls_must_be_https_or_loopback() {
    let c = client("http://127.0.0.1:1");
    let err = c
        .put_presigned("http://evil.example/k?X-Amz-Signature=SEGREDO", b"x")
        .unwrap_err();
    assert!(matches!(err, NetError::Config(_)));
    assert!(!err.to_string().contains("SEGREDO"));
    assert!(c.get_presigned("javascript:alert(1)").is_err());
}

#[test]
fn connection_failures_are_network_errors_without_the_address() {
    // Bind then drop, so the port is closed.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let c = client(&format!("http://127.0.0.1:{port}"));
    let err = c.health().unwrap_err();
    assert!(matches!(err, NetError::Network(_)), "{err:?}");
    assert!(!err.to_string().contains("127.0.0.1"));
}

#[test]
fn constructor_rejects_insecure_urls() {
    let id = Identity::generate("A").unwrap();
    assert!(WorkerClient::new("http://exemplo.dev", id.clone()).is_err());
    assert!(WorkerClient::new("https://exemplo.dev", id.clone()).is_ok());
    assert!(WorkerClient::new("http://localhost:8787", id).is_ok());
}
