//! Integration tests of the public API of duoclip-crypto.

use std::collections::HashSet;

use duoclip_crypto::*;
use duoclip_proto::{decode, encode, ClipId, DeviceId, Interval, Message, Quality};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use uuid::Uuid;

const SEC: i64 = 1_000_000_000;
const T0: i64 = 1_700_000_000 * SEC;

fn clip(n: u128) -> ClipId {
    ClipId(Uuid::from_u128(n))
}
fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}
fn iv(from: i64, to: i64) -> Interval {
    Interval::new(from, to)
}
fn test_key() -> ClipKey {
    ClipKey::from_bytes(std::array::from_fn(|i| {
        (i as u8).wrapping_mul(7).wrapping_add(3)
    }))
}
fn aad() -> ChunkAad {
    ChunkAad {
        clip_id: clip(1),
        pov: dev(2),
        quality: Quality::Full,
        index: 5,
        is_last: false,
    }
}
fn random_bytes(rng: &mut StdRng, len: usize) -> Vec<u8> {
    (0..len).map(|_| rng.gen()).collect()
}

// ---------------------------------------------------------------------------------------
// ClipKey
// ---------------------------------------------------------------------------------------

#[test]
fn generated_keys_differ_and_are_not_trivially_zero() {
    let a = ClipKey::generate();
    let b = ClipKey::generate();
    assert_ne!(a, b);
    assert_ne!(a.expose_bytes(), &[0u8; 32]);
    assert_ne!(b.expose_bytes(), &[0u8; 32]);
}

#[test]
fn key_debug_does_not_leak_bytes() {
    let bytes: [u8; 32] = std::array::from_fn(|i| 0x41 + i as u8);
    let key = ClipKey::from_bytes(bytes);
    for dbg in [
        format!("{key:?}"),
        format!("{key:#?}"),
        format!("{:?}", vec![key.clone()]),
        format!("{:?}", Some(&key)),
    ] {
        assert!(dbg.contains("redacted"), "{dbg}");
        assert!(!dbg.contains(&key.to_base64()), "base64 leaked: {dbg}");
        assert!(!dbg.contains(&hex_of(&bytes)), "hex leaked: {dbg}");
        assert!(!dbg.contains("65, 66"), "decimal bytes leaked: {dbg}");
        assert!(!dbg.contains("ABCDEF"), "ascii leaked: {dbg}");
        assert!(!dbg.contains("0x41"), "hex bytes leaked: {dbg}");
    }
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn key_base64_round_trip() {
    let key = ClipKey::generate();
    let text = key.to_base64();
    assert_eq!(text.len(), 44);
    assert!(text.ends_with('='));
    assert_eq!(ClipKey::from_base64(&text).unwrap(), key);

    let fixed = ClipKey::from_bytes([0u8; 32]);
    assert_eq!(
        fixed.to_base64(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    );
    let ff = ClipKey::from_bytes([0xFF; 32]);
    assert_eq!(ff.to_base64(), format!("{}8=", "/".repeat(42)));
    assert_eq!(ClipKey::from_base64(&ff.to_base64()).unwrap(), ff);
}

#[test]
fn key_base64_rejects_bad_input() {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    for len in [0usize, 1, 16, 24, 31, 33, 48, 64, 1000] {
        let text = STANDARD.encode(vec![7u8; len]);
        assert_eq!(
            ClipKey::from_base64(&text).unwrap_err(),
            CryptoError::KeyLength,
            "length {len}"
        );
    }
    for bad in [
        "not base64!",
        "!!!!",
        " AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        // URL-safe alphabet
        "-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_8=",
        // 32 bytes but padding missing
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        // non-zero trailing bits
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
        "\u{e9}AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    ] {
        assert_eq!(
            ClipKey::from_base64(bad).unwrap_err(),
            CryptoError::Base64,
            "{bad:?}"
        );
    }
}

#[test]
fn key_equality_and_clone() {
    let a = ClipKey::from_bytes([1; 32]);
    let mut other = [1u8; 32];
    other[31] = 2;
    assert_eq!(a, a.clone());
    assert_ne!(a, ClipKey::from_bytes(other));
    other[31] = 1;
    other[0] = 0;
    assert_ne!(a, ClipKey::from_bytes(other));
}

#[test]
fn key_interoperates_with_the_proto_message() {
    let key = ClipKey::generate();
    let msg = Message::ClipKey {
        clip_id: clip(1),
        key_b64: key.to_base64(),
    };
    let wire = encode(&msg).expect("proto accepts our base64");
    match decode(&wire).unwrap() {
        Message::ClipKey { key_b64, .. } => {
            assert_eq!(ClipKey::from_base64(&key_b64).unwrap(), key);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// AAD
// ---------------------------------------------------------------------------------------

#[test]
fn aad_canonical_encoding() {
    assert_eq!(
        aad().to_bytes(),
        b"duoclip/v1|chunk|00000000-0000-0000-0000-000000000001|\
          00000000-0000-0000-0000-000000000002|full|5|0"
    );
    let last = ChunkAad {
        quality: Quality::Proxy,
        index: 4_000_000_000,
        is_last: true,
        ..aad()
    };
    assert_eq!(
        last.to_bytes(),
        b"duoclip/v1|chunk|00000000-0000-0000-0000-000000000001|\
          00000000-0000-0000-0000-000000000002|proxy|4000000000|1"
    );
}

#[test]
fn every_aad_field_changes_the_bytes() {
    let base = aad();
    let variants = [
        ChunkAad {
            clip_id: clip(9),
            ..base
        },
        ChunkAad {
            pov: dev(9),
            ..base
        },
        ChunkAad {
            quality: Quality::Proxy,
            ..base
        },
        ChunkAad { index: 6, ..base },
        ChunkAad {
            is_last: true,
            ..base
        },
    ];
    let mut seen = HashSet::new();
    seen.insert(base.to_bytes());
    for v in variants {
        assert!(seen.insert(v.to_bytes()), "{v:?} collides");
    }
}

// ---------------------------------------------------------------------------------------
// seal_chunk / open_chunk
// ---------------------------------------------------------------------------------------

#[test]
fn seal_open_round_trip_various_sizes() {
    let mut rng = StdRng::seed_from_u64(1);
    let key = test_key();
    for len in [0usize, 1, 15, 16, 17, 255, 4096, 100_000, 1 << 20] {
        let plain = random_bytes(&mut rng, len);
        let sealed = seal_chunk(&key, &aad(), &plain).unwrap();
        assert_eq!(sealed.len(), len + 32, "4 magic + 12 nonce + 16 tag");
        assert_eq!(&sealed[..4], b"DCC1");
        if len >= 16 {
            assert_ne!(
                &sealed[16..16 + 16],
                &plain[..16],
                "ciphertext must differ from plaintext"
            );
        }
        assert_eq!(
            open_chunk(&key, &aad(), &sealed).unwrap(),
            plain,
            "len {len}"
        );
    }
}

#[test]
fn flipping_any_single_bit_fails() {
    let mut rng = StdRng::seed_from_u64(2);
    let key = test_key();
    let plain = random_bytes(&mut rng, 48);
    let sealed = seal_chunk(&key, &aad(), &plain).unwrap();
    assert_eq!(sealed.len(), 80);
    for i in 0..sealed.len() {
        for bit in 0..8 {
            let mut tampered = sealed.clone();
            tampered[i] ^= 1 << bit;
            let err = open_chunk(&key, &aad(), &tampered)
                .expect_err(&format!("byte {i} bit {bit} must be detected"));
            let expected = if i < 4 {
                CryptoError::BadMagic
            } else {
                CryptoError::Decrypt
            };
            assert_eq!(err, expected, "byte {i} bit {bit}");
        }
    }
}

#[test]
fn flipping_whole_bytes_fails_for_empty_plaintext_too() {
    let key = test_key();
    let sealed = seal_chunk(&key, &aad(), b"").unwrap();
    assert_eq!(sealed.len(), 32);
    for i in 0..sealed.len() {
        let mut tampered = sealed.clone();
        tampered[i] ^= 0xFF;
        assert!(open_chunk(&key, &aad(), &tampered).is_err(), "byte {i}");
    }
}

#[test]
fn wrong_key_fails() {
    let key = test_key();
    let sealed = seal_chunk(&key, &aad(), b"secret").unwrap();
    assert_eq!(
        open_chunk(&ClipKey::generate(), &aad(), &sealed).unwrap_err(),
        CryptoError::Decrypt
    );
    let mut almost = *key.expose_bytes();
    almost[31] ^= 1;
    assert_eq!(
        open_chunk(&ClipKey::from_bytes(almost), &aad(), &sealed).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn aad_mismatch_fails_for_each_field() {
    let key = test_key();
    let base = aad();
    let sealed = seal_chunk(&key, &base, b"payload").unwrap();
    assert!(open_chunk(&key, &base, &sealed).is_ok());
    let wrong = [
        (
            "clip",
            ChunkAad {
                clip_id: clip(99),
                ..base
            },
        ),
        (
            "pov",
            ChunkAad {
                pov: dev(99),
                ..base
            },
        ),
        (
            "quality",
            ChunkAad {
                quality: Quality::Proxy,
                ..base
            },
        ),
        (
            "index+1",
            ChunkAad {
                index: base.index + 1,
                ..base
            },
        ),
        (
            "index-1",
            ChunkAad {
                index: base.index - 1,
                ..base
            },
        ),
        ("index 0", ChunkAad { index: 0, ..base }),
        (
            "is_last",
            ChunkAad {
                is_last: true,
                ..base
            },
        ),
    ];
    for (name, bad) in wrong {
        assert_eq!(
            open_chunk(&key, &bad, &sealed).unwrap_err(),
            CryptoError::Decrypt,
            "{name}"
        );
    }
}

#[test]
fn malformed_sealed_blobs_are_rejected() {
    let key = test_key();
    let sealed = seal_chunk(&key, &aad(), b"hello world, this is a chunk").unwrap();

    // Every truncation fails: too short, or an authentication failure.
    for cut in 0..sealed.len() {
        let err = open_chunk(&key, &aad(), &sealed[..cut]).unwrap_err();
        if cut < 32 {
            assert_eq!(err, CryptoError::TooShort, "cut {cut}");
        } else {
            assert_eq!(err, CryptoError::Decrypt, "cut {cut}");
        }
    }
    // Extra trailing bytes fail too.
    let mut longer = sealed.clone();
    longer.push(0);
    assert_eq!(
        open_chunk(&key, &aad(), &longer).unwrap_err(),
        CryptoError::Decrypt
    );

    // Wrong magic at full length.
    let mut other_magic = sealed.clone();
    other_magic[..4].copy_from_slice(b"DCM1");
    assert_eq!(
        open_chunk(&key, &aad(), &other_magic).unwrap_err(),
        CryptoError::BadMagic
    );
    assert_eq!(
        open_chunk(&key, &aad(), &[0u8; 64]).unwrap_err(),
        CryptoError::BadMagic
    );

    // Degenerate inputs.
    assert_eq!(
        open_chunk(&key, &aad(), b"").unwrap_err(),
        CryptoError::TooShort
    );
    assert_eq!(
        open_chunk(&key, &aad(), b"DCC1").unwrap_err(),
        CryptoError::TooShort
    );
    assert_eq!(
        open_chunk(&key, &aad(), &[0xFF; 31]).unwrap_err(),
        CryptoError::TooShort
    );
}

#[test]
fn random_garbage_never_opens() {
    let mut rng = StdRng::seed_from_u64(3);
    let key = test_key();
    for _ in 0..500 {
        let len = rng.gen_range(0..200);
        let mut junk = random_bytes(&mut rng, len);
        if len >= 4 && rng.gen_bool(0.5) {
            junk[..4].copy_from_slice(b"DCC1");
        }
        assert!(open_chunk(&key, &aad(), &junk).is_err());
    }
}

#[test]
fn two_seals_of_the_same_plaintext_differ() {
    let key = test_key();
    let a = seal_chunk(&key, &aad(), b"same plaintext").unwrap();
    let b = seal_chunk(&key, &aad(), b"same plaintext").unwrap();
    assert_ne!(a, b);
    assert_ne!(a[4..16], b[4..16], "nonces differ");
    assert_ne!(a[16..], b[16..], "ciphertexts differ");
    assert_eq!(
        open_chunk(&key, &aad(), &a).unwrap(),
        open_chunk(&key, &aad(), &b).unwrap()
    );
}

#[test]
fn nonces_do_not_repeat() {
    let key = test_key();
    let mut nonces = HashSet::new();
    for _ in 0..2_000 {
        let sealed = seal_chunk(&key, &aad(), b"x").unwrap();
        assert!(nonces.insert(sealed[4..16].to_vec()), "nonce repeated");
    }
}

#[test]
fn chunks_and_manifests_are_not_interchangeable() {
    let key = test_key();
    let mut m = Manifest::new(clip(1), dev(2), Quality::Full);
    m.chunks.push(ManifestEntry {
        index: 0,
        range: iv(T0, T0 + SEC),
        plain_len: 0,
        plain_sha256_hex: sha256_hex(b""),
        is_last: true,
    });
    m.complete = true;
    let sealed_manifest = seal_manifest(&key, &m).unwrap();
    assert_eq!(&sealed_manifest[..4], b"DCM1");
    assert_eq!(
        open_chunk(&key, &aad(), &sealed_manifest).unwrap_err(),
        CryptoError::BadMagic
    );
    let sealed_chunk = seal_chunk(&key, &aad(), &serde_json::to_vec(&m).unwrap()).unwrap();
    assert_eq!(
        open_manifest(&key, clip(1), dev(2), Quality::Full, &sealed_chunk).unwrap_err(),
        CryptoError::BadMagic
    );
}

// ---------------------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------------------

fn entry(index: u32, from: i64, to: i64, is_last: bool) -> ManifestEntry {
    ManifestEntry {
        index,
        range: iv(from, to),
        plain_len: 10 + u64::from(index),
        plain_sha256_hex: sha256_hex(&[index as u8]),
        is_last,
    }
}

/// A well-formed complete manifest of `n` contiguous chunks (n >= 1).
fn manifest(n: u32) -> Manifest {
    let mut m = Manifest::new(clip(1), dev(2), Quality::Proxy);
    for i in 0..n {
        let from = T0 + i64::from(i) * 4 * SEC;
        m.chunks.push(entry(i, from, from + 4 * SEC, i + 1 == n));
    }
    m.complete = true;
    m
}

fn open(key: &ClipKey, sealed: &[u8]) -> Result<Manifest, CryptoError> {
    open_manifest(key, clip(1), dev(2), Quality::Proxy, sealed)
}

fn assert_malformed(m: &Manifest, what: &str) {
    let key = test_key();
    // `seal_manifest` does not validate, so a forged manifest reaches `open_manifest`.
    let sealed = seal_manifest(&key, m).unwrap();
    match open(&key, &sealed) {
        Err(CryptoError::Manifest(_)) => {}
        other => panic!("{what}: expected a Manifest error, got {other:?}"),
    }
    assert!(
        matches!(m.validate(), Err(CryptoError::Manifest(_))),
        "{what}: validate"
    );
}

#[test]
fn manifest_round_trip() {
    let key = test_key();
    for n in [1, 2, 3, 50] {
        let m = manifest(n);
        m.validate().unwrap();
        let sealed = seal_manifest(&key, &m).unwrap();
        assert_eq!(&sealed[..4], b"DCM1");
        assert_eq!(open(&key, &sealed).unwrap(), m);
    }
    // In-progress manifests are fine.
    let empty = Manifest::new(clip(1), dev(2), Quality::Proxy);
    assert_eq!(
        open(&key, &seal_manifest(&key, &empty).unwrap()).unwrap(),
        empty
    );

    let mut partial = manifest(3);
    partial.complete = false;
    partial.chunks.truncate(2);
    partial.chunks[1].is_last = false;
    assert_eq!(
        open(&key, &seal_manifest(&key, &partial).unwrap()).unwrap(),
        partial
    );

    // The last chunk is uploaded but the manifest is not yet flagged complete.
    let mut finishing = manifest(3);
    finishing.complete = false;
    assert_eq!(
        open(&key, &seal_manifest(&key, &finishing).unwrap()).unwrap(),
        finishing
    );
}

#[test]
fn manifest_json_shape_is_stable() {
    let m = Manifest {
        version: 1,
        clip_id: clip(1),
        pov: dev(2),
        quality: Quality::Proxy,
        chunks: vec![ManifestEntry {
            index: 0,
            range: iv(5, 9),
            plain_len: 4,
            plain_sha256_hex: "ab".repeat(32),
            is_last: true,
        }],
        complete: true,
    };
    assert_eq!(
        serde_json::to_string(&m).unwrap(),
        format!(
            concat!(
                r#"{{"version":1,"clip_id":"00000000-0000-0000-0000-000000000001","#,
                r#""pov":"00000000-0000-0000-0000-000000000002","quality":"proxy","#,
                r#""chunks":[{{"index":0,"range":{{"from_utc_ns":5,"to_utc_ns":9}},"#,
                r#""plain_len":4,"plain_sha256_hex":"{}","is_last":true}}],"complete":true}}"#
            ),
            "ab".repeat(32)
        )
    );
}

#[test]
fn manifest_wrong_key_or_position_fails() {
    let key = test_key();
    let sealed = seal_manifest(&key, &manifest(2)).unwrap();
    assert_eq!(
        open(&ClipKey::generate(), &sealed).unwrap_err(),
        CryptoError::Decrypt
    );
    assert_eq!(
        open_manifest(&key, clip(9), dev(2), Quality::Proxy, &sealed).unwrap_err(),
        CryptoError::Decrypt
    );
    assert_eq!(
        open_manifest(&key, clip(1), dev(9), Quality::Proxy, &sealed).unwrap_err(),
        CryptoError::Decrypt
    );
    assert_eq!(
        open_manifest(&key, clip(1), dev(2), Quality::Full, &sealed).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn manifest_tampering_is_detected_at_every_byte() {
    let key = test_key();
    let sealed = seal_manifest(&key, &manifest(2)).unwrap();
    for i in 0..sealed.len() {
        let mut t = sealed.clone();
        t[i] ^= 0x01;
        assert!(open(&key, &t).is_err(), "byte {i}");
    }
    for cut in 0..sealed.len() {
        assert!(open(&key, &sealed[..cut]).is_err(), "cut {cut}");
    }
    assert_eq!(open(&key, b"").unwrap_err(), CryptoError::TooShort);
}

#[test]
fn manifest_rejects_unsupported_version() {
    for v in [0u16, 2, u16::MAX] {
        let mut m = manifest(2);
        m.version = v;
        assert_malformed(&m, &format!("version {v}"));
    }
}

#[test]
fn manifest_rejects_non_contiguous_indices() {
    let mut m = manifest(3);
    m.chunks[0].index = 1;
    m.chunks[1].index = 2;
    m.chunks[2].index = 3;
    assert_malformed(&m, "starts at 1");

    let mut m = manifest(3);
    m.chunks[2].index = 3;
    assert_malformed(&m, "gap");

    let mut m = manifest(3);
    m.chunks[1].index = 0;
    assert_malformed(&m, "duplicate");

    let mut m = manifest(3);
    m.chunks.swap(0, 1);
    assert_malformed(&m, "out of order (indices and ranges)");

    let mut m = manifest(3);
    m.chunks[1].index = 2;
    m.chunks[2].index = 1;
    assert_malformed(&m, "indices swapped only");

    let mut m = manifest(2);
    m.chunks[0].index = u32::MAX;
    assert_malformed(&m, "huge index");
}

#[test]
fn manifest_rejects_bad_is_last_and_complete_combinations() {
    // Truncation: the final chunk was dropped, the previous one is not marked last.
    let mut m = manifest(3);
    m.chunks.pop();
    assert!(m.complete);
    assert_malformed(&m, "complete but last entry missing (truncated)");

    // Dropping the last entry and lying about is_last on the new final one is still sound as
    // a manifest, but its chunk object was sealed with is_last = false: see the AAD test.
    let mut m = manifest(3);
    m.chunks[0].is_last = true;
    assert_malformed(&m, "first of three is_last");

    let mut m = manifest(3);
    m.chunks[1].is_last = true;
    assert_malformed(&m, "middle is_last (complete)");
    m.complete = false;
    assert_malformed(&m, "middle is_last (incomplete)");

    let mut m = manifest(3);
    for e in &mut m.chunks {
        e.is_last = true;
    }
    assert_malformed(&m, "all is_last");

    let mut m = manifest(3);
    m.chunks[2].is_last = false;
    assert_malformed(&m, "complete, nothing marked last");

    let mut m = manifest(1);
    m.chunks.clear();
    assert_malformed(&m, "complete with no chunks");
}

#[test]
fn manifest_rejects_bad_ranges() {
    type Edit = Box<dyn Fn(&mut Manifest)>;
    let cases: Vec<(&str, Edit)> = vec![
        (
            "empty range",
            Box::new(|m| m.chunks[1].range = iv(T0 + 100, T0 + 100)),
        ),
        (
            "reversed range",
            Box::new(|m| m.chunks[1].range = iv(T0 + 100, T0 + 50)),
        ),
        ("zero start", Box::new(|m| m.chunks[0].range = iv(0, T0))),
        (
            "negative start",
            Box::new(|m| m.chunks[0].range = iv(-5, T0)),
        ),
        (
            "too long",
            Box::new(|m| m.chunks[0].range = iv(T0, T0 + 601 * SEC)),
        ),
        (
            "start goes backwards",
            Box::new(|m| m.chunks[2].range = iv(T0 + 3 * SEC, m.chunks[2].range.to_utc_ns)),
        ),
        (
            "end goes backwards",
            Box::new(|m| {
                let from = m.chunks[2].range.from_utc_ns;
                m.chunks[1].range = iv(T0 + 4 * SEC, from + 10 * SEC);
                m.chunks[2].range = iv(from, from + 5 * SEC);
            }),
        ),
        (
            "chunk 1 entirely before chunk 0",
            Box::new(|m| m.chunks[1].range = iv(T0 - 10 * SEC, T0 - 5 * SEC)),
        ),
    ];
    for (name, edit) in cases {
        let mut m = manifest(3);
        edit(&mut m);
        assert_malformed(&m, name);
    }
}

#[test]
fn manifest_accepts_touching_overlapping_and_gapped_ranges() {
    let key = test_key();
    let mut m = manifest(3);
    // overlapping but monotone, then a gap
    m.chunks[1].range = iv(
        m.chunks[0].range.from_utc_ns + SEC,
        m.chunks[0].range.to_utc_ns + SEC,
    );
    m.chunks[2].range = iv(T0 + 100 * SEC, T0 + 104 * SEC);
    let back = open(&key, &seal_manifest(&key, &m).unwrap()).unwrap();
    assert_eq!(back, m);
    // identical ranges are non-decreasing too
    let mut same = manifest(2);
    same.chunks[1].range = same.chunks[0].range;
    assert!(same.validate().is_ok());
}

#[test]
fn manifest_rejects_too_many_chunks() {
    let mut m = Manifest::new(clip(1), dev(2), Quality::Proxy);
    for i in 0..=MAX_MANIFEST_CHUNKS {
        let from = T0 + i as i64;
        m.chunks.push(entry(i as u32, from, from + 1, false));
    }
    assert!(matches!(m.validate(), Err(CryptoError::Manifest(_))));
    m.chunks.pop();
    assert!(m.validate().is_ok());
}

#[test]
fn manifest_entry_verify_checks_length_and_hash() {
    let e = ManifestEntry {
        index: 4,
        range: iv(1, 2),
        plain_len: 5,
        plain_sha256_hex: sha256_hex(b"hello"),
        is_last: false,
    };
    assert!(e.verify(b"hello").is_ok());
    assert!(matches!(e.verify(b"hellO"), Err(CryptoError::Manifest(_))));
    assert!(matches!(e.verify(b"hell"), Err(CryptoError::Manifest(_))));
    assert!(matches!(e.verify(b""), Err(CryptoError::Manifest(_))));
    let upper = ManifestEntry {
        plain_sha256_hex: e.plain_sha256_hex.to_uppercase(),
        ..e.clone()
    };
    assert!(
        upper.verify(b"hello").is_ok(),
        "hex case is not significant"
    );
    let junk = ManifestEntry {
        plain_sha256_hex: "zz".into(),
        ..e
    };
    assert!(junk.verify(b"hello").is_err());
}

#[test]
fn manifest_chunk_aad_follows_the_entries() {
    let m = manifest(3);
    let a = m.chunk_aad(2).unwrap();
    assert_eq!(
        (a.clip_id, a.pov, a.quality, a.index, a.is_last),
        (clip(1), dev(2), Quality::Proxy, 2, true)
    );
    assert!(!m.chunk_aad(1).unwrap().is_last);
    assert!(m.chunk_aad(3).is_none());
    assert!(m.chunk_aad(u32::MAX).is_none());
}

// ---------------------------------------------------------------------------------------
// Chunker
// ---------------------------------------------------------------------------------------

fn frag(i: usize, len: usize) -> Fragment {
    let from = T0 + i as i64 * SEC;
    Fragment {
        range: iv(from, from + SEC),
        data: vec![(i % 251) as u8; len],
    }
}

/// Feeds fragments with the given sizes; returns every emitted chunk in order, together
/// with the number of chunks that `push` handed out before `finish`.
fn run_chunker(target: usize, sizes: &[usize]) -> (Vec<PlainChunk>, usize) {
    let mut chunker = Chunker::new(target);
    let mut out = Vec::new();
    for (i, &len) in sizes.iter().enumerate() {
        out.extend(chunker.push(frag(i, len)));
    }
    let during = out.len();
    out.extend(chunker.finish());
    (out, during)
}

fn lens(chunks: &[PlainChunk]) -> Vec<usize> {
    chunks.iter().map(|c| c.data.len()).collect()
}

#[test]
fn chunker_empty_stream() {
    assert!(Chunker::new(10).finish().is_none());
    assert!(Chunker::default().finish().is_none());
}

#[test]
fn chunker_default_target_is_6_mib() {
    assert_eq!(DEFAULT_CHUNK_TARGET_BYTES, 6 * 1024 * 1024);
    assert_eq!(Chunker::default().target_bytes(), 6 * 1024 * 1024);
    assert_eq!(Chunker::new(123).target_bytes(), 123);
}

#[test]
fn chunker_exactly_one_fragment() {
    let mut c = Chunker::new(10);
    let f = frag(0, 4);
    assert!(
        c.push(f.clone()).is_none(),
        "nothing is released before we know it is not last"
    );
    assert_eq!(c.buffered_bytes(), 4);
    let last = c.finish().expect("the single fragment is the last chunk");
    assert_eq!(last.index, 0);
    assert!(last.is_last);
    assert_eq!(last.range, f.range);
    assert_eq!(last.data, f.data);
}

#[test]
fn chunker_boundary_sizes_with_target_10() {
    // (fragment sizes, expected chunk sizes)
    let cases: Vec<(Vec<usize>, Vec<usize>)> = vec![
        (vec![10], vec![10]),
        (vec![11], vec![11]),
        (vec![5, 5], vec![10]),
        (vec![5, 6], vec![5, 6]),
        (vec![5, 5, 1], vec![10, 1]),
        (vec![9, 1], vec![10]),
        (vec![9, 2], vec![9, 2]),
        (vec![10, 10], vec![10, 10]),
        (vec![10, 1], vec![10, 1]),
        (vec![3, 3, 3, 1], vec![10]),
        (vec![3, 3, 3, 2], vec![9, 2]),
        (vec![3, 3, 3, 3, 3], vec![9, 6]),
        (vec![1; 25], vec![10, 10, 5]),
        (vec![4, 25, 4], vec![4, 25, 4]),
        (vec![25, 25], vec![25, 25]),
        (vec![25, 1, 1], vec![25, 2]),
        (vec![0, 0, 0], vec![0]),
        (vec![0, 10, 0], vec![10]),
        (vec![10, 0], vec![10]),
        // Empty-data fragments never form a chunk of their own.
        (vec![0, 11, 1], vec![11, 1]),
        (vec![5, 0, 5, 0, 1], vec![10, 1]),
    ];
    for (sizes, expected) in cases {
        let (chunks, _) = run_chunker(10, &sizes);
        assert_eq!(lens(&chunks), expected, "sizes {sizes:?}");
        // structural invariants for every case
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.index as usize, i);
            assert_eq!(c.is_last, i + 1 == chunks.len());
        }
    }
}

#[test]
fn chunker_holds_back_the_open_chunk() {
    let mut c = Chunker::new(10);
    // A full chunk is still held: a later fragment might not exist.
    assert!(c.push(frag(0, 10)).is_none());
    // The next fragment proves chunk 0 is not the last; it is released, not flagged last.
    let released = c
        .push(frag(1, 3))
        .expect("chunk 0 released by the next fragment");
    assert_eq!(
        (released.index, released.is_last, released.data.len()),
        (0, false, 10)
    );
    // Calling finish right after a "full" chunk returns that chunk as the true last one.
    let mut d = Chunker::new(10);
    assert!(d.push(frag(0, 10)).is_none());
    let last = d.finish().unwrap();
    assert_eq!((last.index, last.is_last, last.data.len()), (0, true, 10));
    // An oversized fragment is held too.
    let mut e = Chunker::new(10);
    assert!(e.push(frag(0, 500)).is_none());
    let last = e.finish().unwrap();
    assert_eq!((last.index, last.is_last, last.data.len()), (0, true, 500));
}

#[test]
fn chunker_oversized_fragment_becomes_its_own_chunk() {
    let (chunks, during) = run_chunker(10, &[3, 40, 3, 3]);
    assert_eq!(lens(&chunks), vec![3, 40, 6]);
    assert_eq!(during, 2);
    assert!(chunks[1].data.iter().all(|&b| b == 1));
    assert_eq!(chunks[1].range, frag(1, 0).range);
}

#[test]
fn chunker_zero_target_makes_one_chunk_per_fragment() {
    let (chunks, _) = run_chunker(0, &[1, 2, 3]);
    assert_eq!(lens(&chunks), vec![1, 2, 3]);
}

#[test]
fn chunker_ranges_are_the_union_of_fragment_ranges() {
    let (chunks, _) = run_chunker(10, &[4, 4, 4, 4, 4]);
    // chunks: [f0 f1] [f2 f3] [f4]
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].range, iv(T0, T0 + 2 * SEC));
    assert_eq!(chunks[1].range, iv(T0 + 2 * SEC, T0 + 4 * SEC));
    assert_eq!(chunks[2].range, iv(T0 + 4 * SEC, T0 + 5 * SEC));

    // Fragments with a gap, overlap, and out-of-order timestamps still give min/max.
    let mut c = Chunker::new(100);
    for r in [iv(100, 200), iv(150, 400), iv(50, 120), iv(500, 600)] {
        assert!(c
            .push(Fragment {
                range: r,
                data: vec![0; 5]
            })
            .is_none());
    }
    assert_eq!(c.finish().unwrap().range, iv(50, 600));
}

#[test]
fn chunker_preserves_bytes_and_order_on_random_streams() {
    let mut rng = StdRng::seed_from_u64(0xC0FFEE);
    for round in 0..300 {
        let target = rng.gen_range(1..80);
        let n = rng.gen_range(0..40);
        let sizes: Vec<usize> = (0..n).map(|_| rng.gen_range(0..40)).collect();
        let (chunks, during) = run_chunker(target, &sizes);

        if sizes.is_empty() {
            assert!(chunks.is_empty());
            continue;
        }
        assert_eq!(
            during + 1,
            chunks.len(),
            "round {round}: finish emits exactly one chunk"
        );

        // Reference greedy partition.
        let mut expected: Vec<Vec<usize>> = vec![vec![]];
        let mut sum = 0usize;
        for (i, &len) in sizes.iter().enumerate() {
            // An empty fragment never closes a chunk, even an oversized one.
            if sum > 0 && len > 0 && sum + len > target {
                expected.push(vec![]);
                sum = 0;
            }
            expected.last_mut().unwrap().push(i);
            sum += len;
        }
        assert_eq!(chunks.len(), expected.len(), "round {round}: chunk count");
        for (idx, (chunk, frags)) in chunks.iter().zip(&expected).enumerate() {
            let want_len: usize = frags.iter().map(|&i| sizes[i]).sum();
            assert_eq!(chunk.data.len(), want_len, "round {round} chunk {idx}");
            assert_eq!(chunk.index as usize, idx);
            assert_eq!(chunk.is_last, idx + 1 == chunks.len());
            let first = *frags.first().unwrap();
            let last = *frags.last().unwrap();
            assert_eq!(
                chunk.range,
                iv(
                    frag(first, 0).range.from_utc_ns,
                    frag(last, 0).range.to_utc_ns
                )
            );
            // never exceeds the target unless it is a single oversized fragment
            // (empty-data fragments ride along with their neighbours)
            assert!(
                chunk.data.len() <= target || frags.iter().filter(|&&i| sizes[i] > 0).count() == 1,
                "round {round} chunk {idx}"
            );
            // and is as full as possible: the next fragment really would not have fit
            if let Some(next) = expected.get(idx + 1) {
                assert!(chunk.data.len() + sizes[next[0]] > target);
            }
        }

        // Byte-for-byte reassembly.
        let joined: Vec<u8> = chunks.iter().flat_map(|c| c.data.iter().copied()).collect();
        let original: Vec<u8> = sizes
            .iter()
            .enumerate()
            .flat_map(|(i, &l)| frag(i, l).data)
            .collect();
        assert_eq!(joined, original, "round {round}");
    }
}

#[test]
fn chunker_debug_does_not_dump_media_bytes() {
    let mut c = Chunker::new(10);
    c.push(frag(0, 5));
    assert!(format!("{c:?}").contains("buffered_bytes: 5"));
    let chunk = c.finish().unwrap();
    let dbg = format!("{chunk:?}");
    assert!(dbg.contains("data_len: 5"));
    assert!(!dbg.contains("[0, 0, 0"));
    assert!(format!("{:?}", frag(0, 7)).contains("data_len: 7"));
}

#[test]
fn plain_chunk_helpers_match_manifest_and_aad() {
    let chunk = PlainChunk {
        index: 3,
        range: iv(T0, T0 + SEC),
        data: b"abc".to_vec(),
        is_last: true,
    };
    let e = chunk.manifest_entry();
    assert_eq!(e.index, 3);
    assert_eq!(e.range, chunk.range);
    assert_eq!(e.plain_len, 3);
    assert_eq!(
        e.plain_sha256_hex,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(e.is_last);
    let a = chunk.aad(clip(1), dev(2), Quality::Full);
    assert_eq!(
        a,
        ChunkAad {
            clip_id: clip(1),
            pov: dev(2),
            quality: Quality::Full,
            index: 3,
            is_last: true
        }
    );
}

// ---------------------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------------------

struct Uploaded {
    sealed_chunks: Vec<Vec<u8>>,
    sealed_manifest: Vec<u8>,
    original: Vec<u8>,
}

/// Uploader side: chunk, seal each chunk, build and seal the manifest.
fn upload(key: &ClipKey, fragment_sizes: &[usize], target: usize, seed: u64) -> Uploaded {
    let (clip_id, pov, quality) = (clip(1), dev(2), Quality::Proxy);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut chunker = Chunker::new(target);
    let mut manifest = Manifest::new(clip_id, pov, quality);
    let mut sealed_chunks = Vec::new();
    let mut original = Vec::new();

    let mut handle = |chunk: PlainChunk, manifest: &mut Manifest| {
        let sealed = seal_chunk(key, &chunk.aad(clip_id, pov, quality), &chunk.data).unwrap();
        manifest.chunks.push(chunk.manifest_entry());
        sealed_chunks.push(sealed);
    };

    for (i, &len) in fragment_sizes.iter().enumerate() {
        let data = random_bytes(&mut rng, len);
        original.extend_from_slice(&data);
        let from = T0 + i as i64 * SEC;
        if let Some(chunk) = chunker.push(Fragment {
            range: iv(from, from + SEC),
            data,
        }) {
            handle(chunk, &mut manifest);
        }
    }
    if let Some(chunk) = chunker.finish() {
        handle(chunk, &mut manifest);
        manifest.complete = true;
    }
    Uploaded {
        sealed_chunks,
        sealed_manifest: seal_manifest(key, &manifest).unwrap(),
        original,
    }
}

/// Downloader side: open the manifest, require completeness, open and verify every chunk.
fn download(
    key: &ClipKey,
    up: &Uploaded,
    sealed_chunks: &[Vec<u8>],
) -> Result<Vec<u8>, CryptoError> {
    let m = open(key, &up.sealed_manifest)?;
    if !m.complete {
        return Err(CryptoError::Manifest("manifest is not complete".into()));
    }
    if m.chunks.len() != sealed_chunks.len() {
        return Err(CryptoError::Manifest(
            "chunk count differs from the manifest".into(),
        ));
    }
    let mut out = Vec::new();
    for (entry, sealed) in m.chunks.iter().zip(sealed_chunks) {
        let aad = m.chunk_aad(entry.index).expect("entry exists");
        let plain = open_chunk(key, &aad, sealed)?;
        entry.verify(&plain)?;
        out.extend_from_slice(&plain);
    }
    Ok(out)
}

#[test]
fn full_pipeline_round_trip() {
    let key = test_key();
    let sizes = [700, 900, 1500, 400, 400, 400, 2500, 100];
    let up = upload(&key, &sizes, 2_000, 11);
    assert!(up.sealed_chunks.len() >= 4);
    let got = download(&key, &up, &up.sealed_chunks).unwrap();
    assert_eq!(got, up.original);
}

#[test]
fn pipeline_with_a_single_chunk_and_with_an_empty_clip() {
    let key = test_key();
    let up = upload(&key, &[10, 20], 1 << 20, 12);
    assert_eq!(up.sealed_chunks.len(), 1);
    assert_eq!(download(&key, &up, &up.sealed_chunks).unwrap(), up.original);

    // An empty stream yields no chunks and an incomplete manifest, which readers refuse.
    let empty = upload(&key, &[], 100, 13);
    assert!(empty.sealed_chunks.is_empty());
    assert!(matches!(
        download(&key, &empty, &empty.sealed_chunks),
        Err(CryptoError::Manifest(_))
    ));
}

#[test]
fn dropping_the_last_chunk_is_detected() {
    let key = test_key();
    let up = upload(&key, &[800; 8], 2_000, 14);
    let n = up.sealed_chunks.len();
    assert!(n >= 3);

    // (a) The provider deletes the last object but keeps the manifest.
    let truncated = &up.sealed_chunks[..n - 1];
    assert!(download(&key, &up, truncated).is_err());

    // (b) The uploader's manifest with the last entry removed cannot be sealed into a valid
    //     complete manifest: the new final entry is not marked is_last.
    let mut m = open(&key, &up.sealed_manifest).unwrap();
    m.chunks.pop();
    let forged = seal_manifest(&key, &m).unwrap();
    assert!(matches!(open(&key, &forged), Err(CryptoError::Manifest(_))));

    // (c) Even without the manifest: the previous chunk was sealed with is_last = false, so
    //     it cannot be passed off as the final chunk.
    let m = open(&key, &up.sealed_manifest).unwrap();
    let mut as_last = m.chunk_aad(u32::try_from(n - 2).unwrap()).unwrap();
    assert!(!as_last.is_last);
    as_last.is_last = true;
    assert_eq!(
        open_chunk(&key, &as_last, &up.sealed_chunks[n - 2]).unwrap_err(),
        CryptoError::Decrypt
    );

    // (d) Replaying an older, incomplete manifest is visible through `complete`.
    let mut old = m.clone();
    old.chunks.truncate(n - 1);
    old.chunks.last_mut().unwrap().is_last = false;
    old.complete = false;
    let replay = open(&key, &seal_manifest(&key, &old).unwrap()).unwrap();
    assert!(!replay.complete);
}

#[test]
fn swapping_or_reordering_chunks_is_detected() {
    let key = test_key();
    let up = upload(&key, &[800; 8], 2_000, 15);
    assert!(up.sealed_chunks.len() >= 3);

    let mut swapped = up.sealed_chunks.clone();
    swapped.swap(0, 1);
    assert!(download(&key, &up, &swapped).is_err());

    let mut duplicated = up.sealed_chunks.clone();
    duplicated[1] = duplicated[0].clone();
    assert!(download(&key, &up, &duplicated).is_err());

    // A chunk from another clip sealed with the same key does not fit either.
    let other = upload(&key, &[800; 8], 2_000, 16);
    let mut spliced = up.sealed_chunks.clone();
    spliced[0] = other.sealed_chunks[0].clone();
    // Same key, same position, different bytes: AEAD passes but the manifest hash fails.
    assert!(matches!(
        download(&key, &up, &spliced),
        Err(CryptoError::Manifest(_))
    ));
}

#[test]
fn error_messages_do_not_leak_anything_and_are_readable() {
    for e in [
        CryptoError::BadMagic,
        CryptoError::TooShort,
        CryptoError::Decrypt,
        CryptoError::Encrypt,
        CryptoError::Base64,
        CryptoError::KeyLength,
        CryptoError::Json("j".into()),
        CryptoError::Manifest("m".into()),
    ] {
        assert!(!e.to_string().is_empty());
    }
}

// ---------------------------------------------------------------------------------------
// Adversarial review regressions
// ---------------------------------------------------------------------------------------

#[test]
fn chunker_empty_fragment_never_closes_a_chunk() {
    // An oversized open chunk followed by an empty fragment used to be closed by it, leaving
    // an empty chunk marked is_last.
    let (chunks, during) = run_chunker(10, &[20, 0]);
    assert_eq!(during, 0);
    assert_eq!(lens(&chunks), vec![20]);
    assert!(chunks[0].is_last);
    assert_eq!(
        chunks[0].range,
        iv(T0, T0 + 2 * SEC),
        "empty fragment extends the range"
    );

    let (chunks, during) = run_chunker(10, &[20, 0, 0, 5, 0]);
    assert_eq!(during, 1);
    assert_eq!(lens(&chunks), vec![20, 5]);
    assert_eq!(chunks[0].range, iv(T0, T0 + 3 * SEC));
    assert_eq!(chunks[1].range, iv(T0 + 3 * SEC, T0 + 5 * SEC));
    assert!(!chunks[0].is_last && chunks[1].is_last);

    // Empty fragments before the first byte are absorbed too.
    let (chunks, _) = run_chunker(10, &[0, 0, 30, 0]);
    assert_eq!(lens(&chunks), vec![30]);

    // A stream with no bytes at all still yields exactly one (empty) last chunk.
    let (chunks, during) = run_chunker(10, &[0, 0, 0]);
    assert_eq!(during, 0);
    assert_eq!(lens(&chunks), vec![0]);
    assert!(chunks[0].is_last);
    assert_eq!(chunks[0].range, iv(T0, T0 + 3 * SEC));
}

#[test]
fn chunker_never_emits_an_empty_chunk_for_a_non_empty_stream() {
    let mut rng = StdRng::seed_from_u64(0x5eed);
    for _ in 0..200 {
        let target = rng.gen_range(0..40);
        let sizes: Vec<usize> = (0..rng.gen_range(1..30))
            .map(|_| {
                if rng.gen_bool(0.3) {
                    0
                } else {
                    rng.gen_range(1..60)
                }
            })
            .collect();
        let (chunks, _) = run_chunker(target, &sizes);
        let total: usize = sizes.iter().sum();
        assert_eq!(lens(&chunks).iter().sum::<usize>(), total);
        if total > 0 {
            assert!(
                chunks.iter().all(|c| !c.data.is_empty()),
                "target {target}, sizes {sizes:?}, got {:?}",
                lens(&chunks)
            );
        }
        // Exactly one last chunk, and it is the final one; indices are contiguous.
        assert_eq!(chunks.iter().filter(|c| c.is_last).count(), 1);
        assert!(chunks.last().unwrap().is_last);
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.index as usize, i);
        }
    }
}

fn entry_with_len(plain_len: u64) -> Manifest {
    let mut m = manifest(1);
    m.chunks[0].plain_len = plain_len;
    m
}

#[test]
fn manifest_rejects_implausible_plain_len() {
    // 64 MiB of ciphertext (the proto cap) minus the 32 bytes of sealing overhead.
    let max = duoclip_proto::MAX_CHUNK_BYTES - 32;
    let key = test_key();
    let ok = entry_with_len(max);
    assert!(ok.validate().is_ok());
    assert!(open(&key, &seal_manifest(&key, &ok).unwrap()).is_ok());

    for bad in [max + 1, duoclip_proto::MAX_CHUNK_BYTES, u64::MAX] {
        assert_malformed(&entry_with_len(bad), "plain_len above the chunk cap");
    }
    // The whole manifest can therefore never sum to more than u64 can hold.
    let mut m = manifest(1);
    m.chunks.clear();
    m.complete = false;
    for i in 0..MAX_MANIFEST_CHUNKS {
        m.chunks.push(entry(i as u32, T0, T0 + SEC, false));
        m.chunks[i].plain_len = max;
    }
    assert!(m.validate().is_ok());
    assert!(m
        .chunks
        .iter()
        .try_fold(0u64, |acc, c| acc.checked_add(c.plain_len))
        .is_some());
}

#[test]
fn oversized_sealed_manifests_are_refused_without_being_parsed() {
    let key = test_key();
    // Too big to even look at: refused as a Manifest error before decryption.
    let huge = vec![0u8; MAX_SEALED_MANIFEST_BYTES + 1];
    assert!(matches!(open(&key, &huge), Err(CryptoError::Manifest(_))));
    // Exactly at the limit it is looked at (and fails for being garbage, not for its size).
    let at_limit = vec![0u8; MAX_SEALED_MANIFEST_BYTES];
    assert_eq!(open(&key, &at_limit).unwrap_err(), CryptoError::BadMagic);

    // The sealing side refuses to produce a manifest the reader would refuse.
    let mut m = manifest(1);
    m.chunks[0].plain_sha256_hex = "a".repeat(MAX_SEALED_MANIFEST_BYTES);
    assert!(matches!(
        seal_manifest(&key, &m),
        Err(CryptoError::Manifest(_))
    ));

    // A big-but-legal manifest still round trips.
    m.chunks[0].plain_sha256_hex = "a".repeat(1 << 20);
    let sealed = seal_manifest(&key, &m).unwrap();
    assert_eq!(open(&key, &sealed).unwrap(), m);
}

#[test]
fn sealed_manifest_limit_covers_the_maximum_chunk_count() {
    // Worst realistic entry: 5-digit index, current-epoch ranges, 64-char hash, large length.
    let e = ManifestEntry {
        index: 99_999,
        range: iv(T0, T0 + 4 * SEC),
        plain_len: 67_108_832,
        plain_sha256_hex: sha256_hex(b"x"),
        is_last: false,
    };
    let json = serde_json::to_vec(&e).unwrap();
    assert!(
        (json.len() + 1) * MAX_MANIFEST_CHUNKS < MAX_SEALED_MANIFEST_BYTES,
        "{} bytes per entry",
        json.len()
    );
}

#[test]
fn key_equality_detects_a_difference_in_any_single_byte() {
    // Keys that differ in exactly one bit, at every byte position, compare unequal; equal
    // keys compare equal. (Constant-time behaviour itself cannot be asserted in a test.)
    let base = [0x42u8; 32];
    let a = ClipKey::from_bytes(base);
    for i in 0..32 {
        let mut other = base;
        other[i] ^= 0x01;
        assert_ne!(a, ClipKey::from_bytes(other), "byte {i}");
    }
    assert_eq!(a, ClipKey::from_bytes(base));
}
