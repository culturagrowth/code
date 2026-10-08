//! Wire types of `POST /v1/presence` (must match `worker/src/validate.ts` and `worker/src/routes.ts`
//! exactly) and their validation.

use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{Presence, MAX_GAME_ID_BYTES};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::PresenceError;

/// Largest integer JavaScript represents exactly (`Number.MAX_SAFE_INTEGER`, 2^53 - 1). The
/// Worker rejects larger values, and larger values it would send could not be trusted.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Body of `POST /v1/presence`. Every field is always serialized (`None` as `null`), in the
/// Worker's order. Device identity comes from the request signature, never from the body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HeartbeatRequest {
    /// games-db id of the foreground game, or `None` (app open, no game).
    pub game: Option<String>,
    /// Lowercase hyphenated uuid of the crew I am committed to, or `None`.
    pub active_crew: Option<String>,
    /// AppClock ms since when I hold a seat, or `None` when not seated.
    pub seated_since_ms: Option<u64>,
    /// Strictly increasing within one run of the app.
    pub seq: u64,
    /// AppClock ms identifying this run of the app (never smaller than a previous run's).
    pub online_since_ms: u64,
}

impl HeartbeatRequest {
    /// Serializes the body after checking it against the Worker validation: integers must be
    /// JavaScript safe integers, `game` must be a valid game id and `active_crew` a canonical
    /// uuid. Requests built by [`crate::PresenceClient::poll`] always pass.
    pub fn to_json(&self) -> Result<String, PresenceError> {
        for (field, value) in [
            ("seq", Some(self.seq)),
            ("online_since_ms", Some(self.online_since_ms)),
            ("seated_since_ms", self.seated_since_ms),
        ] {
            if value.is_some_and(|v| v > MAX_SAFE_INTEGER) {
                return Err(PresenceError::UnsafeInteger(field));
            }
        }
        if self.game.as_deref().is_some_and(|g| !is_valid_game_id(g)) {
            return Err(PresenceError::InvalidRequest("game"));
        }
        if self
            .active_crew
            .as_deref()
            .is_some_and(|c| parse_canonical_uuid(c).is_none())
        {
            return Err(PresenceError::InvalidRequest("active_crew"));
        }
        serde_json::to_string(self).map_err(|_| PresenceError::InvalidRequest("serialization"))
    }
}

/// `200` body of `POST /v1/presence`. Unknown fields are ignored. Crew and member entries that
/// do not fit the expected shape are skipped one by one; use [`HeartbeatResponse::from_json`]
/// to also drop entries with invalid content (non-canonical uuids, bad game ids, unsafe
/// integers) and to refuse a malformed top-level body.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct HeartbeatResponse {
    /// Always `true` on success.
    pub ok: bool,
    /// Worker receipt time of this heartbeat (Worker clock; never used as the local clock).
    pub seen_at_ms: u64,
    /// `seen_at_ms` + the Worker TTL (diagnostics only).
    pub expires_at: u64,
    /// Requested heartbeat period; the client clamps it (see `SPEC.md`).
    pub heartbeat_interval_ms: u64,
    /// One complete snapshot per crew of the caller, sorted by crew id.
    #[serde(deserialize_with = "lenient_vec")]
    pub crews: Vec<CrewSnapshot>,
}

/// Every fresh and available member of one crew (the caller included).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CrewSnapshot {
    /// Lowercase hyphenated crew uuid.
    pub crew_id: String,
    /// Members sorted by device id.
    #[serde(deserialize_with = "lenient_vec")]
    pub members: Vec<MemberPresence>,
}

/// One member row, as returned by the Worker (`GET /v1/crews/:crew/presence` shape).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct MemberPresence {
    /// Lowercase hyphenated device uuid.
    pub device_id: String,
    /// Display name of the device (UI only).
    pub display_name: String,
    /// games-db id, or `None` (app open, no game).
    pub game: Option<String>,
    /// The crew the member is committed to, or `None`.
    pub active_crew: Option<String>,
    /// AppClock ms since when it holds a seat, or `None`.
    pub seated_since_ms: Option<u64>,
    /// Sequence within its run.
    pub seq: u64,
    /// AppClock ms identifying its run.
    pub online_since_ms: u64,
    /// Worker receipt time of its last heartbeat (Worker clock; diagnostics only).
    pub seen_at_ms: u64,
    /// `seen_at_ms` + the Worker TTL (diagnostics only).
    pub expires_at: u64,
}

impl HeartbeatResponse {
    /// Parses a `200` body. Errors if the body is not a JSON object with `ok: true`, the four
    /// integers as JavaScript safe integers and `crews` as an array. Invalid crew or member
    /// entries are dropped individually (never failing the whole snapshot).
    pub fn from_json(body: &[u8]) -> Result<Self, PresenceError> {
        let mut resp: Self = serde_json::from_slice(body)
            .map_err(|e| PresenceError::MalformedResponse(e.to_string()))?;
        if !resp.ok {
            return Err(PresenceError::NotOk);
        }
        for (field, value) in [
            ("seen_at_ms", resp.seen_at_ms),
            ("expires_at", resp.expires_at),
            ("heartbeat_interval_ms", resp.heartbeat_interval_ms),
        ] {
            if value > MAX_SAFE_INTEGER {
                return Err(PresenceError::UnsafeInteger(field));
            }
        }
        resp.crews
            .retain(|c| parse_canonical_uuid(&c.crew_id).is_some());
        for crew in &mut resp.crews {
            crew.members.retain(|m| m.to_presence().is_some());
        }
        Ok(resp)
    }

    /// Every valid member of every valid crew snapshot, as session presences (a device in
    /// several of my crews appears once per crew; `apply_snapshot` merges them).
    pub fn presences(&self) -> Vec<Presence> {
        self.crews
            .iter()
            .filter(|c| parse_canonical_uuid(&c.crew_id).is_some())
            .flat_map(|c| c.members.iter())
            .filter_map(MemberPresence::to_presence)
            .collect()
    }
}

impl MemberPresence {
    /// Converts a member row into a session presence, or `None` if any field is invalid:
    /// non-canonical uuids, a game id the Worker would reject (or longer than
    /// [`MAX_GAME_ID_BYTES`]), or an integer above [`MAX_SAFE_INTEGER`].
    pub fn to_presence(&self) -> Option<Presence> {
        let device = DeviceId(parse_canonical_uuid(&self.device_id)?);
        let active_crew = match &self.active_crew {
            Some(c) => Some(CrewId(parse_canonical_uuid(c)?)),
            None => None,
        };
        if self.game.as_deref().is_some_and(|g| !is_valid_game_id(g)) {
            return None;
        }
        let integers = [
            Some(self.seq),
            Some(self.online_since_ms),
            Some(self.seen_at_ms),
            Some(self.expires_at),
            self.seated_since_ms,
        ];
        if integers.iter().flatten().any(|v| *v > MAX_SAFE_INTEGER) {
            return None;
        }
        Some(Presence {
            device,
            seq: self.seq,
            game: self.game.clone(),
            active_crew,
            seated_since_ms: self.seated_since_ms,
            online_since_ms: self.online_since_ms,
        })
    }
}

/// Deserializes a JSON array, dropping (instead of failing on) elements that do not fit `T`.
fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| T::deserialize(v).ok())
        .collect())
}

/// Parses exactly the canonical lowercase hyphenated 8-4-4-4-12 form (the only form the Worker
/// accepts and emits); any other spelling is refused.
pub fn parse_canonical_uuid(s: &str) -> Option<Uuid> {
    let canonical = s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || matches!(b, b'a'..=b'f'),
        });
    if canonical {
        Uuid::parse_str(s).ok()
    } else {
        None
    }
}

/// Mirrors the Worker's `game` validation: nonempty after trimming whitespace, at most
/// [`MAX_GAME_ID_BYTES`] UTF-8 bytes, no control or bidi-control characters.
pub fn is_valid_game_id(g: &str) -> bool {
    let blank = g.chars().all(|c| c.is_whitespace() || c == '\u{FEFF}');
    let forbidden = |c: char| {
        c.is_control()
            || ('\u{202A}'..='\u{202E}').contains(&c)
            || ('\u{2066}'..='\u{2069}').contains(&c)
    };
    !blank && g.len() <= MAX_GAME_ID_BYTES && !g.chars().any(forbidden)
}
