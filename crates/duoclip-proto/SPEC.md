# duoclip-proto — SPEC

Control protocol exchanged between DuoClip apps over the authenticated WebRTC data channel
(and, as a fallback, relayed by the Worker). Small JSON messages, strictly validated.
See `docs/pesquisa-app-clipes-sincronizados.md` sections 6, 8, 9 and 10 for context.

## Conventions (shared by all crates)

- Time is `i64` nanoseconds.
  - `*_utc_ns`: UTC, POSIX nanoseconds (leap seconds not counted), from the app's global clock.
  - `*_local_ns`: the sender's own monotonic clock (QPC-derived on Windows). Only meaningful to the sender,
    except in the time ping/pong exchange.
- IDs are UUID v4.
- Intervals are half-open: `[from, to)`, and `from < to` must hold.
- No `unsafe`. No panics on untrusted input: decoding/validation return `Result`.

## Public API (must exist, names may gain extra items)

```rust
pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(transparent)]
pub struct DeviceId(pub Uuid);
#[derive(... same ...)] #[serde(transparent)]
pub struct ClipId(pub Uuid);
#[derive(... same ...)] #[serde(transparent)]
pub struct CrewId(pub Uuid);           // a group of paired friends

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Quality { Proxy, Full }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interval { pub from_utc_ns: i64, pub to_utc_ns: i64 }   // half-open

pub struct ClipRequest {
    pub clip_id: ClipId,
    pub requester: DeviceId,
    pub seq: u32,                    // incremented on every resend/extension by the requester
    pub hotkey_utc_ns: i64,          // press time on the GLOBAL clock (not "now")
    pub pre_ms: u32,                 // seconds before the press (default 30_000)
    pub post_ms: u32,                // seconds after the press (default 10_000)
    pub margin_pre_ms: u32,          // hidden slack for the editor (default 2_000)
    pub margin_post_ms: u32,         // (default 2_000)
    pub max_len_ms: u32,             // cap including extensions (default 180_000)
    pub sync_uncertainty_ns: i64,    // requester's current clock bound (>= 0)
    pub requester_no_source: bool,   // requester has no game running but peers should still save
}
impl ClipRequest {
    pub fn window_utc(&self) -> Interval;   // [hotkey - pre - margin_pre, hotkey + post + margin_post)
    pub fn validate(&self) -> Result<(), ProtoError>;
}

pub enum AckStage { Pinned, Ready, Rejected { reason: RejectReason } }
pub enum RejectReason { NotInSession, NoSource, MemoryBudget, TooManyActiveClips, InvalidRequest, Other(String) }

pub struct ClipAck {
    pub clip_id: ClipId,
    pub from: DeviceId,
    pub stage: AckStage,
    pub coverage: Vec<Interval>,       // what this device actually has (utc)
    pub expected_bytes: Option<u64>,
    pub proxy_available: bool,
    pub truncated_by_source_end: bool,
}

pub struct ChunkRef {
    pub clip_id: ClipId,
    pub pov: DeviceId,
    pub quality: Quality,
    pub index: u32,
    pub is_last: bool,
    pub object_key: String,            // must start with "clips/", <= 512 bytes, [A-Za-z0-9/_.-] only, no "..", no "//"
    pub range: Interval,
    pub size_bytes: u64,               // ciphertext size
    pub sha256_hex: String,            // 64 lowercase hex chars, of the ciphertext object
}

#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello { device_id: DeviceId, crew_id: CrewId, app_version: String, protocol_version: u16 },
    TimePing { seq: u64, t1_local_ns: i64 },
    TimePong { seq: u64, t1_local_ns: i64, t2_local_ns: i64, t3_local_ns: i64 },
    ClipRequest(ClipRequest),
    ClipAck(ClipAck),
    ClipExtend { clip_id: ClipId, requester: DeviceId, seq: u32, new_hotkey_utc_ns: i64 },
    RangeRequest { clip_id: ClipId, pov: DeviceId, quality: Quality, range: Interval },
    ChunkAvailable(ChunkRef),
    ClipKey { clip_id: ClipId, key_b64: String },     // standard base64 of exactly 32 bytes
    ClipDelete { clip_id: ClipId },
    Bye,
}

#[derive(Serialize, Deserialize)]
pub struct Envelope { pub v: u16, pub msg: Message }

pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtoError>;   // JSON of Envelope{v: PROTOCOL_VERSION, msg}; validates first
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError>;     // size check, JSON parse, version check, validate

pub fn object_key(crew: CrewId, clip: ClipId, pov: DeviceId, quality: Quality, index: u32) -> String;
//   "clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin"  (uuids lowercase hyphenated)
pub fn manifest_key(crew: CrewId, clip: ClipId, pov: DeviceId, quality: Quality) -> String;
//   "clips/{crew}/{clip}/{pov}/{proxy|full}/manifest.bin"
pub fn clip_prefix(crew: CrewId, clip: ClipId) -> String;      // "clips/{crew}/{clip}/"

#[derive(Debug, thiserror::Error)]
pub enum ProtoError { TooLarge(usize), Json(String), UnsupportedVersion(u16), Invalid(String) }
```

## Validation rules (`validate` on every message, used by `encode` and `decode`)

- `ClipRequest`: `pre_ms <= 120_000`, `post_ms <= 30_000`, each margin `<= 10_000`,
  `max_len_ms <= 180_000`, `pre + post + margins <= max_len_ms`, `sync_uncertainty_ns >= 0`,
  `hotkey_utc_ns > 0`, and the arithmetic in `window_utc` must not overflow (use checked ops).
- `ClipExtend`: `new_hotkey_utc_ns > 0`.
- `Interval`: `from < to`, both `> 0`, length <= 10 minutes.
- `ClipAck`: at most 64 coverage intervals, each valid. `Rejected{Other(s)}`: s <= 200 chars.
- `ChunkRef`: key rules above. `sha256_hex` is 64 lowercase hex chars. `size_bytes <= 64 MiB`.
- `ClipKey`: `key_b64` decodes (standard base64) to exactly 32 bytes.
- `Hello`: `app_version` 1..=32 chars of `[0-9A-Za-z.+-]`.
- `TimePong`: `t3_local_ns >= t2_local_ns`.
- `RangeRequest`: interval valid.

## Tests (required)

- Round-trip every variant through `encode`/`decode`.
- Each validation rule rejects bad input, with at least one negative test per rule.
- Oversized input, wrong version, unknown `type`, and garbage bytes are all rejected without panicking.
- `object_key`/`manifest_key` format is stable (golden strings).
- `cargo clippy -p duoclip-proto --all-targets -- -D warnings` is clean and `cargo fmt` is applied.

## Implementation notes (accepted deviations, Phase A review)

- IDs (`ClipId`, `DeviceId`, `CrewId`) deserialize ONLY from canonical 36-char lowercase hyphenated text, so each id has
  exactly one wire form. This matches the Worker and the object-key/AAD canonical form.
- `ClipRequest::validate` also rejects an empty window (pre + post + margins == 0). `ClipExtend` uses the same overflow rules as `ClipRequest`.
- `decode` probes `v` first: another protocol version returns `UnsupportedVersion` even if the body is unknown.
  JSON error text is truncated to 200 chars. Unknown fields are rejected (`deny_unknown_fields`).
- `Message`'s Debug redacts `ClipKey.key_b64`.
- `ChunkRef::verify_object_key(crew)`: receivers MUST check that a `ChunkRef`'s key matches `object_key(...)` built from its own fields.
- Nanosecond timestamps exceed 2^53, so JavaScript consumers (the Worker) must treat them as opaque strings or BigInt.
