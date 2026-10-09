//! DuoClip Worker client and the `duoclip-amigos` setup tool (see `SPEC.md`).
//!
//! - [`Identity`]: this PC's Ed25519 key, device id and display name, stored in
//!   `%APPDATA%\DuoClip\identidade.json`.
//! - [`signing`]: request signing, identical to `worker/src/auth.ts`.
//! - [`WorkerClient`]: blocking calls for devices, groups, invites, presence and clips.
//! - [`cli`]: the `duoclip-amigos` commands (`configurar`, `criar-grupo`, `convidar`, `entrar`,
//!   `grupos`, `status`).
//!
//! Secrets (the device key), signatures and presigned URLs are never printed, logged or put
//! in error messages.

#![forbid(unsafe_code)]

pub mod cli;
mod client;
mod error;
mod identity;
pub mod signing;

pub use client::{
    normalize_base_url, DeleteOutcome, DownloadRequest, Invite, Member, PresignedChunk,
    PresignedObject, PresignedUrls, UploadRequest, WorkerClient,
};
pub use error::NetError;
pub use identity::{
    validate_crew_name, validate_display_name, GroupEntry, Identity, IDENTITY_FILE_NAME,
    MAX_CREW_NAME_CHARS, MAX_DISPLAY_NAME_CHARS,
};
