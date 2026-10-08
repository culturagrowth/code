//! Message types of the control protocol and their validation rules.

use std::fmt;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::error::{ensure, ProtoError};
use crate::keys::validate_object_key;
use crate::types::{ClipId, CrewId, DeviceId, Interval, Quality};

/// Default time kept before the hotkey press, in milliseconds.
pub const DEFAULT_PRE_MS: u32 = 30_000;
/// Default time kept after the hotkey press, in milliseconds.
pub const DEFAULT_POST_MS: u32 = 10_000;
/// Default hidden slack before the pre-roll, in milliseconds.
pub const DEFAULT_MARGIN_PRE_MS: u32 = 2_000;
/// Default hidden slack after the post-roll, in milliseconds.
pub const DEFAULT_MARGIN_POST_MS: u32 = 2_000;
/// Default cap on the total clip length including extensions, in milliseconds.
pub const DEFAULT_MAX_LEN_MS: u32 = 180_000;

/// Largest accepted `pre_ms`.
pub const MAX_PRE_MS: u32 = 120_000;
/// Largest accepted `post_ms`.
pub const MAX_POST_MS: u32 = 30_000;
/// Largest accepted `margin_pre_ms` / `margin_post_ms`.
pub const MAX_MARGIN_MS: u32 = 10_000;
/// Largest accepted `max_len_ms`.
pub const MAX_REQUEST_LEN_MS: u32 = 180_000;

/// Most coverage intervals one [`ClipAck`] may carry.
pub const MAX_ACK_COVERAGE_INTERVALS: usize = 64;
/// Longest accepted free-text rejection reason, in characters.
pub const MAX_OTHER_REASON_CHARS: usize = 200;
/// Longest accepted `app_version`, in characters.
pub const MAX_APP_VERSION_CHARS: usize = 32;
/// Largest accepted ciphertext size of one chunk: 64 MiB.
pub const MAX_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

/// Length of a SHA-256 digest rendered as hex.
const SHA256_HEX_LEN: usize = 64;
/// Exact length, in bytes, of a clip key.
const CLIP_KEY_BYTES: usize = 32;
/// Length of the standard (padded) base64 text of 32 bytes.
const CLIP_KEY_B64_LEN: usize = 44;

const NS_PER_MS: i64 = 1_000_000;

/// "Save everything around this instant" request, sent by the player who pressed the hotkey.
///
/// All durations are milliseconds; all instants are on the global UTC clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipRequest {
    /// Identifies the clip across all POVs.
    pub clip_id: ClipId,
    /// Device that pressed the hotkey.
    pub requester: DeviceId,
    /// Incremented by the requester on every resend or extension.
    pub seq: u32,
    /// Press time on the GLOBAL clock (not "now").
    pub hotkey_utc_ns: i64,
    /// Milliseconds kept before the press (default [`DEFAULT_PRE_MS`]).
    pub pre_ms: u32,
    /// Milliseconds kept after the press (default [`DEFAULT_POST_MS`]).
    pub post_ms: u32,
    /// Hidden slack before the pre-roll, for the editor (default [`DEFAULT_MARGIN_PRE_MS`]).
    pub margin_pre_ms: u32,
    /// Hidden slack after the post-roll, for the editor (default [`DEFAULT_MARGIN_POST_MS`]).
    pub margin_post_ms: u32,
    /// Cap on the whole clip length including extensions (default [`DEFAULT_MAX_LEN_MS`]).
    pub max_len_ms: u32,
    /// The requester's current clock error bound in nanoseconds (`>= 0`).
    pub sync_uncertainty_ns: i64,
    /// The requester has no game running but peers should still save.
    pub requester_no_source: bool,
}

/// Nanoseconds covered by `a_ms + b_ms` milliseconds. Cannot overflow: at most about 8.6e15.
fn span_ns(a_ms: u32, b_ms: u32) -> i64 {
    (i64::from(a_ms) + i64::from(b_ms)) * NS_PER_MS
}

impl ClipRequest {
    /// Builds a request with the default pre/post/margin/max-length values and
    /// `requester_no_source = false`.
    pub fn with_defaults(
        clip_id: ClipId,
        requester: DeviceId,
        hotkey_utc_ns: i64,
        sync_uncertainty_ns: i64,
    ) -> Self {
        Self {
            clip_id,
            requester,
            seq: 0,
            hotkey_utc_ns,
            pre_ms: DEFAULT_PRE_MS,
            post_ms: DEFAULT_POST_MS,
            margin_pre_ms: DEFAULT_MARGIN_PRE_MS,
            margin_post_ms: DEFAULT_MARGIN_POST_MS,
            max_len_ms: DEFAULT_MAX_LEN_MS,
            sync_uncertainty_ns,
            requester_no_source: false,
        }
    }

    /// The UTC window every peer must save:
    /// `[hotkey - pre - margin_pre, hotkey + post + margin_post)`.
    ///
    /// Saturates instead of overflowing for absurd inputs; [`ClipRequest::validate`] rejects
    /// those via [`ClipRequest::checked_window_utc`].
    pub fn window_utc(&self) -> Interval {
        Interval::new(
            self.hotkey_utc_ns
                .saturating_sub(span_ns(self.pre_ms, self.margin_pre_ms)),
            self.hotkey_utc_ns
                .saturating_add(span_ns(self.post_ms, self.margin_post_ms)),
        )
    }

    /// Same as [`ClipRequest::window_utc`] but returns `None` if any bound overflows `i64`.
    pub fn checked_window_utc(&self) -> Option<Interval> {
        let from = self
            .hotkey_utc_ns
            .checked_sub(span_ns(self.pre_ms, self.margin_pre_ms))?;
        let to = self
            .hotkey_utc_ns
            .checked_add(span_ns(self.post_ms, self.margin_post_ms))?;
        Some(Interval::new(from, to))
    }

    /// Checks every rule: `pre_ms <= 120_000`, `post_ms <= 30_000`, each margin `<= 10_000`,
    /// `max_len_ms <= 180_000`, `pre + post + margins <= max_len_ms`, a non-empty window,
    /// `sync_uncertainty_ns >= 0`, `hotkey_utc_ns > 0`, and no overflow in the window bounds.
    pub fn validate(&self) -> Result<(), ProtoError> {
        ensure(self.pre_ms <= MAX_PRE_MS, "pre_ms exceeds 120000")?;
        ensure(self.post_ms <= MAX_POST_MS, "post_ms exceeds 30000")?;
        ensure(
            self.margin_pre_ms <= MAX_MARGIN_MS,
            "margin_pre_ms exceeds 10000",
        )?;
        ensure(
            self.margin_post_ms <= MAX_MARGIN_MS,
            "margin_post_ms exceeds 10000",
        )?;
        ensure(
            self.max_len_ms <= MAX_REQUEST_LEN_MS,
            "max_len_ms exceeds 180000",
        )?;
        let total_ms = u64::from(self.pre_ms)
            + u64::from(self.post_ms)
            + u64::from(self.margin_pre_ms)
            + u64::from(self.margin_post_ms);
        ensure(
            total_ms <= u64::from(self.max_len_ms),
            "pre + post + margins exceeds max_len_ms",
        )?;
        ensure(total_ms > 0, "request window is empty")?;
        ensure(
            self.sync_uncertainty_ns >= 0,
            "sync_uncertainty_ns must be >= 0",
        )?;
        ensure(self.hotkey_utc_ns > 0, "hotkey_utc_ns must be > 0")?;
        ensure(
            self.checked_window_utc().is_some(),
            "request window overflows i64 nanoseconds",
        )
    }
}

/// Why a peer refused to save a clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The peer is not in an active session with the requester.
    NotInSession,
    /// The peer has no game/source being recorded.
    NoSource,
    /// Pinning the window would exceed the peer's memory budget.
    MemoryBudget,
    /// The peer already holds the maximum number of active clips.
    TooManyActiveClips,
    /// The request failed validation.
    InvalidRequest,
    /// Anything else; free text of at most [`MAX_OTHER_REASON_CHARS`] characters.
    Other(String),
}

/// Progress stage reported by a [`ClipAck`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AckStage {
    /// The window is pinned in the peer's buffer (ACK 1).
    Pinned,
    /// The preview is encrypted and available (ACK 2).
    Ready,
    /// The peer will not save this clip.
    Rejected {
        /// Why the peer refused.
        reason: RejectReason,
    },
}

/// A peer's answer to a [`ClipRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipAck {
    /// The clip being acknowledged.
    pub clip_id: ClipId,
    /// The acknowledging device.
    pub from: DeviceId,
    /// Which stage this ACK reports.
    pub stage: AckStage,
    /// What this device actually has, as UTC intervals (at most 64).
    pub coverage: Vec<Interval>,
    /// Expected total ciphertext size, if already known.
    pub expected_bytes: Option<u64>,
    /// A proxy-quality version is (or will be) available.
    pub proxy_available: bool,
    /// The recording ended (game closed) before the end of the window.
    pub truncated_by_source_end: bool,
}

impl ClipAck {
    /// Checks the coverage list (at most 64 valid intervals) and the free-text reject reason
    /// (at most 200 characters).
    pub fn validate(&self) -> Result<(), ProtoError> {
        ensure(
            self.coverage.len() <= MAX_ACK_COVERAGE_INTERVALS,
            "more than 64 coverage intervals",
        )?;
        for interval in &self.coverage {
            interval.validate()?;
        }
        if let AckStage::Rejected {
            reason: RejectReason::Other(text),
        } = &self.stage
        {
            ensure(
                text.chars().count() <= MAX_OTHER_REASON_CHARS,
                "rejection reason longer than 200 characters",
            )?;
        }
        Ok(())
    }
}

/// Announces that one encrypted chunk is available in the bucket (or over P2P).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkRef {
    /// The clip this chunk belongs to.
    pub clip_id: ClipId,
    /// Whose point of view the chunk contains.
    pub pov: DeviceId,
    /// Quality tier of the chunk.
    pub quality: Quality,
    /// Chunk index, contiguous from 0.
    pub index: u32,
    /// True for the final chunk of this `(clip, pov, quality)`.
    pub is_last: bool,
    /// Bucket key. Must pass [`validate_object_key`].
    pub object_key: String,
    /// UTC time covered by the chunk.
    pub range: Interval,
    /// Ciphertext size in bytes (at most 64 MiB).
    pub size_bytes: u64,
    /// SHA-256 of the ciphertext object: 64 lowercase hex characters.
    pub sha256_hex: String,
}

impl ChunkRef {
    /// Checks the object key, the range, the size cap and the hash format.
    ///
    /// The key is only checked structurally: `ChunkRef` does not carry the crew id, so it
    /// cannot be compared with [`crate::object_key`] here. The receiver MUST additionally call
    /// [`ChunkRef::verify_object_key`] with its own crew id before using the key, otherwise a
    /// peer could announce a chunk of one POV or clip while pointing at another object.
    pub fn validate(&self) -> Result<(), ProtoError> {
        validate_object_key(&self.object_key)?;
        self.range.validate()?;
        ensure(
            self.size_bytes <= MAX_CHUNK_BYTES,
            "chunk size exceeds 64 MiB",
        )?;
        ensure(
            self.sha256_hex.len() == SHA256_HEX_LEN
                && self
                    .sha256_hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')),
            "sha256_hex must be 64 lowercase hex characters",
        )
    }

    /// Checks that `object_key` is exactly the canonical key of this chunk in `crew`, that is
    /// [`crate::object_key`] of `(crew, clip_id, pov, quality, index)`.
    ///
    /// Call this with the receiver's own crew id after [`ChunkRef::validate`]: it ties the
    /// announced key to the announced `clip_id`, `pov`, `quality` and `index`, and to the
    /// crew the receiver belongs to.
    pub fn verify_object_key(&self, crew: CrewId) -> Result<(), ProtoError> {
        let expected =
            crate::keys::object_key(crew, self.clip_id, self.pov, self.quality, self.index);
        ensure(
            self.object_key == expected,
            "object_key does not match the chunk's crew, clip, pov, quality and index",
        )
    }
}

/// Every message of the control protocol. Serialized with an internal `"type"` tag.
///
/// Unknown fields are rejected for every variant that has fields. (serde cannot enforce that
/// for the payload-less `Bye` of an internally tagged enum, so [`crate::decode`] checks it by
/// hand; deserializing a `Message` directly with serde ignores extra fields on `Bye`.)
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    /// First message on a new channel.
    Hello {
        /// Sender's device.
        device_id: DeviceId,
        /// Crew the sender claims to belong to.
        crew_id: CrewId,
        /// Sender's app version, 1..=32 chars of `[0-9A-Za-z.+-]`.
        app_version: String,
        /// Protocol version the sender speaks.
        protocol_version: u16,
    },
    /// Clock-sync probe (NTP-style, first leg).
    TimePing {
        /// Probe sequence number.
        seq: u64,
        /// Sender's local monotonic time when the ping left.
        t1_local_ns: i64,
    },
    /// Clock-sync reply carrying the responder's timestamps.
    TimePong {
        /// Sequence number echoed from the ping.
        seq: u64,
        /// Ping send time (sender's clock), echoed.
        t1_local_ns: i64,
        /// Ping receive time (responder's clock).
        t2_local_ns: i64,
        /// Pong send time (responder's clock); must be `>= t2_local_ns`.
        t3_local_ns: i64,
    },
    /// Ask every peer to save a window around a hotkey press.
    ClipRequest(ClipRequest),
    /// A peer's answer to a clip request.
    ClipAck(ClipAck),
    /// Move the end of an existing clip window to a later press.
    ClipExtend {
        /// Clip being extended.
        clip_id: ClipId,
        /// Device that originally requested the clip.
        requester: DeviceId,
        /// Incremented on every resend/extension.
        seq: u32,
        /// New press time on the global clock.
        new_hotkey_utc_ns: i64,
    },
    /// Ask a peer for a specific range of its POV (typically in full quality).
    RangeRequest {
        /// Clip concerned.
        clip_id: ClipId,
        /// Whose POV is wanted.
        pov: DeviceId,
        /// Wanted quality.
        quality: Quality,
        /// Wanted UTC range.
        range: Interval,
    },
    /// An encrypted chunk is ready.
    ChunkAvailable(ChunkRef),
    /// The per-clip encryption key, sent over the authenticated channel only.
    ClipKey {
        /// Clip the key belongs to.
        clip_id: ClipId,
        /// Standard base64 of exactly 32 bytes.
        key_b64: String,
    },
    /// Delete the clip everywhere.
    ClipDelete {
        /// Clip to delete.
        clip_id: ClipId,
    },
    /// Orderly goodbye.
    Bye,
}

impl Message {
    /// The wire name of the message (the value of its `"type"` tag). Handy for logging.
    pub fn type_name(&self) -> &'static str {
        match self {
            Message::Hello { .. } => "hello",
            Message::TimePing { .. } => "time_ping",
            Message::TimePong { .. } => "time_pong",
            Message::ClipRequest(_) => "clip_request",
            Message::ClipAck(_) => "clip_ack",
            Message::ClipExtend { .. } => "clip_extend",
            Message::RangeRequest { .. } => "range_request",
            Message::ChunkAvailable(_) => "chunk_available",
            Message::ClipKey { .. } => "clip_key",
            Message::ClipDelete { .. } => "clip_delete",
            Message::Bye => "bye",
        }
    }

    /// Applies the validation rules of this message type. Used by `encode` and `decode`.
    pub fn validate(&self) -> Result<(), ProtoError> {
        match self {
            Message::Hello { app_version, .. } => validate_app_version(app_version),
            Message::TimePing { .. } | Message::ClipDelete { .. } | Message::Bye => Ok(()),
            Message::TimePong {
                t2_local_ns,
                t3_local_ns,
                ..
            } => ensure(
                t3_local_ns >= t2_local_ns,
                "t3_local_ns must be >= t2_local_ns",
            ),
            Message::ClipRequest(req) => req.validate(),
            Message::ClipAck(ack) => ack.validate(),
            Message::ClipExtend {
                new_hotkey_utc_ns, ..
            } => validate_extend_hotkey(*new_hotkey_utc_ns),
            Message::RangeRequest { range, .. } => range.validate(),
            Message::ChunkAvailable(chunk) => chunk.validate(),
            Message::ClipKey { key_b64, .. } => validate_key_b64(key_b64),
        }
    }
}

/// `new_hotkey_utc_ns`: `> 0`, and far enough below `i64::MAX` that the extended window
/// (`new_hotkey + post + margin_post`, each at its maximum) cannot overflow. The matching
/// `ClipRequest` is not part of the message, so the worst case is assumed.
fn validate_extend_hotkey(new_hotkey_utc_ns: i64) -> Result<(), ProtoError> {
    ensure(new_hotkey_utc_ns > 0, "new_hotkey_utc_ns must be > 0")?;
    ensure(
        new_hotkey_utc_ns
            .checked_add(span_ns(MAX_POST_MS, MAX_MARGIN_MS))
            .is_some(),
        "new_hotkey_utc_ns is too close to the end of the i64 range",
    )
}

/// `app_version`: 1..=32 characters of `[0-9A-Za-z.+-]`.
fn validate_app_version(v: &str) -> Result<(), ProtoError> {
    // Only ASCII is allowed, so the byte length equals the character count.
    ensure(
        !v.is_empty() && v.len() <= MAX_APP_VERSION_CHARS,
        "app_version must be 1..=32 characters",
    )?;
    ensure(
        v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-')),
        "app_version contains a forbidden character",
    )
}

/// `key_b64`: standard base64 that decodes to exactly 32 bytes.
fn validate_key_b64(s: &str) -> Result<(), ProtoError> {
    ensure(
        s.len() == CLIP_KEY_B64_LEN,
        "key_b64 must be 44 base64 characters",
    )?;
    let bytes = STANDARD
        .decode(s)
        .map_err(|_| ProtoError::invalid("key_b64 is not valid standard base64"))?;
    ensure(
        bytes.len() == CLIP_KEY_BYTES,
        "key_b64 must decode to 32 bytes",
    )
}

/// Manual `Debug` so that the clip key never ends up in logs.
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Hello {
                device_id,
                crew_id,
                app_version,
                protocol_version,
            } => f
                .debug_struct("Hello")
                .field("device_id", device_id)
                .field("crew_id", crew_id)
                .field("app_version", app_version)
                .field("protocol_version", protocol_version)
                .finish(),
            Message::TimePing { seq, t1_local_ns } => f
                .debug_struct("TimePing")
                .field("seq", seq)
                .field("t1_local_ns", t1_local_ns)
                .finish(),
            Message::TimePong {
                seq,
                t1_local_ns,
                t2_local_ns,
                t3_local_ns,
            } => f
                .debug_struct("TimePong")
                .field("seq", seq)
                .field("t1_local_ns", t1_local_ns)
                .field("t2_local_ns", t2_local_ns)
                .field("t3_local_ns", t3_local_ns)
                .finish(),
            Message::ClipRequest(r) => f.debug_tuple("ClipRequest").field(r).finish(),
            Message::ClipAck(a) => f.debug_tuple("ClipAck").field(a).finish(),
            Message::ClipExtend {
                clip_id,
                requester,
                seq,
                new_hotkey_utc_ns,
            } => f
                .debug_struct("ClipExtend")
                .field("clip_id", clip_id)
                .field("requester", requester)
                .field("seq", seq)
                .field("new_hotkey_utc_ns", new_hotkey_utc_ns)
                .finish(),
            Message::RangeRequest {
                clip_id,
                pov,
                quality,
                range,
            } => f
                .debug_struct("RangeRequest")
                .field("clip_id", clip_id)
                .field("pov", pov)
                .field("quality", quality)
                .field("range", range)
                .finish(),
            Message::ChunkAvailable(c) => f.debug_tuple("ChunkAvailable").field(c).finish(),
            Message::ClipKey { clip_id, .. } => f
                .debug_struct("ClipKey")
                .field("clip_id", clip_id)
                .field("key_b64", &"<redacted>")
                .finish(),
            Message::ClipDelete { clip_id } => f
                .debug_struct("ClipDelete")
                .field("clip_id", clip_id)
                .finish(),
            Message::Bye => f.write_str("Bye"),
        }
    }
}
