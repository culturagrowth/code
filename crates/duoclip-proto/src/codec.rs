//! JSON encoding and decoding of envelopes.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;
use crate::message::Message;
use crate::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION};

/// The wire wrapper around every message: `{"v": <version>, "msg": {...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Protocol version of the sender.
    pub v: u16,
    /// The wrapped message.
    pub msg: Message,
}

/// Borrowing twin of [`Envelope`], so `encode` does not clone the message.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    v: u16,
    msg: &'a Message,
}

/// Looks only at the version, so that a message from a newer protocol (which may contain
/// unknown fields or types) is reported as `UnsupportedVersion` rather than as bad JSON.
#[derive(Deserialize)]
struct VersionProbe {
    v: u16,
}

/// Validates `msg` and serializes it as the JSON of `Envelope { v: PROTOCOL_VERSION, msg }`.
///
/// # Errors
///
/// [`ProtoError::Invalid`] if the message breaks a validation rule, and
/// [`ProtoError::TooLarge`] if the result exceeds [`MAX_MESSAGE_BYTES`].
pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    msg.validate()?;
    let bytes = serde_json::to_vec(&EnvelopeRef {
        v: PROTOCOL_VERSION,
        msg,
    })
    .map_err(|e| ProtoError::from_json(&e))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ProtoError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

/// Parses untrusted bytes into a validated [`Message`].
///
/// The checks run in this order: size ([`ProtoError::TooLarge`]), JSON syntax and the `v`
/// field ([`ProtoError::Json`]), version ([`ProtoError::UnsupportedVersion`]), full message
/// shape ([`ProtoError::Json`]), validation rules ([`ProtoError::Invalid`]). Unknown fields
/// are rejected for every message type, `bye` included.
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ProtoError::TooLarge(bytes.len()));
    }
    let probe: VersionProbe =
        serde_json::from_slice(bytes).map_err(|e| ProtoError::from_json(&e))?;
    if probe.v != PROTOCOL_VERSION {
        return Err(ProtoError::UnsupportedVersion(probe.v));
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|e| ProtoError::from_json(&e))?;
    envelope.msg.validate()?;
    if envelope.msg == Message::Bye {
        reject_extra_bye_fields(bytes)?;
    }
    Ok(envelope.msg)
}

/// serde cannot apply `deny_unknown_fields` to the payload-less `Bye` variant of an
/// internally tagged enum, so enforce "the message object has the `type` key and nothing
/// else" by hand. Runs only for `Bye`, on input that already parsed as a valid envelope.
fn reject_extra_bye_fields(bytes: &[u8]) -> Result<(), ProtoError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| ProtoError::from_json(&e))?;
    match value.get("msg").and_then(serde_json::Value::as_object) {
        Some(msg) if msg.len() == 1 => Ok(()),
        _ => Err(ProtoError::invalid("bye message carries unexpected fields")),
    }
}
