//! Identifiers, quality tiers and time intervals.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ensure, ProtoError};

/// Longest interval accepted by [`Interval::validate`]: 10 minutes, in nanoseconds.
pub const MAX_INTERVAL_NS: i64 = 10 * 60 * 1_000_000_000;

/// True if `s` is exactly a lowercase, hyphenated 8-4-4-4-12 UUID: the one text form the
/// Worker, the object keys and the AADs all use.
fn is_canonical_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || matches!(b, b'a'..=b'f'),
        })
}

/// Deserializes a UUID, accepting only the canonical lowercase hyphenated text form in
/// human-readable formats (JSON). The `uuid` crate alone would also accept uppercase,
/// `{braced}`, `urn:uuid:` and un-hyphenated spellings, which would let the same identifier
/// have several wire forms and defeat plain string comparison (for example in a relay).
fn deserialize_canonical_uuid<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    if !d.is_human_readable() {
        return Uuid::deserialize(d);
    }
    struct CanonicalUuid;
    impl Visitor<'_> for CanonicalUuid {
        type Value = Uuid;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a lowercase hyphenated UUID string")
        }

        fn visit_str<E: de::Error>(self, s: &str) -> Result<Uuid, E> {
            if !is_canonical_uuid(s) {
                return Err(E::custom(
                    "UUID must be 36 lowercase hex characters in 8-4-4-4-12 hyphenated form",
                ));
            }
            Uuid::parse_str(s).map_err(E::custom)
        }
    }
    d.deserialize_str(CanonicalUuid)
}

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl<'de> Deserialize<'de> for $name {
            /// Only the canonical lowercase hyphenated text form is accepted.
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                deserialize_canonical_uuid(d).map(Self)
            }
        }

        impl $name {
            /// Generates a fresh random (v4) identifier.
            pub fn new_random() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }

        impl fmt::Display for $name {
            /// Lowercase hyphenated form, identical to the one used in object keys.
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }
    };
}

id_type!(
    /// Identifies one installed DuoClip app instance (one player's PC).
    DeviceId
);
id_type!(
    /// Identifies one clip across all POVs.
    ClipId
);
id_type!(
    /// Identifies a group of paired friends.
    CrewId
);

/// Quality tier of a transferred POV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Low-bitrate preview used by the editor.
    Proxy,
    /// Full quality, fetched only for the range the user picked.
    Full,
}

impl Quality {
    /// Canonical lowercase name used in object keys and AADs: `"proxy"` or `"full"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Proxy => "proxy",
            Quality::Full => "full",
        }
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A half-open UTC interval `[from_utc_ns, to_utc_ns)` in POSIX nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interval {
    /// Inclusive start.
    pub from_utc_ns: i64,
    /// Exclusive end.
    pub to_utc_ns: i64,
}

impl Interval {
    /// Builds an interval without validating it (see [`Interval::validate`]).
    pub fn new(from_utc_ns: i64, to_utc_ns: i64) -> Self {
        Self {
            from_utc_ns,
            to_utc_ns,
        }
    }

    /// Length in nanoseconds, saturating instead of overflowing. Negative for reversed input.
    pub fn len_ns(&self) -> i64 {
        self.to_utc_ns.saturating_sub(self.from_utc_ns)
    }

    /// True if `t` lies in `[from, to)`.
    pub fn contains(&self, t_utc_ns: i64) -> bool {
        self.from_utc_ns <= t_utc_ns && t_utc_ns < self.to_utc_ns
    }

    /// The overlap of two intervals, or `None` if they share no instant.
    pub fn intersection(&self, other: &Interval) -> Option<Interval> {
        let from = self.from_utc_ns.max(other.from_utc_ns);
        let to = self.to_utc_ns.min(other.to_utc_ns);
        (from < to).then_some(Interval::new(from, to))
    }

    /// Checks that `from < to`, both are `> 0`, and the length is at most 10 minutes.
    pub fn validate(&self) -> Result<(), ProtoError> {
        ensure(self.from_utc_ns > 0, "interval start must be > 0")?;
        ensure(self.to_utc_ns > 0, "interval end must be > 0")?;
        ensure(
            self.from_utc_ns < self.to_utc_ns,
            "interval start must be before its end",
        )?;
        // Both bounds are positive here, so the subtraction cannot overflow.
        ensure(
            self.to_utc_ns - self.from_utc_ns <= MAX_INTERVAL_NS,
            "interval longer than 10 minutes",
        )
    }
}
