//! RFC 5905 packet subset for an SNTPv4 client.
//!
//! - [`NtpTimestamp`] converts between the 64-bit NTP format (seconds since 1900 + 2^-32 fraction,
//!   wrapping every 2^32 s ≈ 136 years) and UTC POSIX nanoseconds. Decoding is era-aware: the era
//!   closest to a caller-supplied pivot is chosen, so the 2036 rollover is handled.
//! - [`build_request`] builds a client packet whose transmit timestamp is a random **cookie**
//!   rather than the real time (privacy, and an off-path attacker cannot forge a reply). The
//!   client measures `t1`/`t4` on its own monotonic clock.
//! - [`parse_response`] validates a server reply (see its docs for the rejection rules).

use std::fmt;

/// Seconds between the NTP epoch (1900-01-01) and the POSIX epoch (1970-01-01).
pub const NTP_UNIX_OFFSET_SECS: i64 = 2_208_988_800;
/// Maximum accepted root distance (`root_delay/2 + root_dispersion`): 1.5 s.
pub const MAX_ROOT_DISTANCE_NS: i64 = 1_500_000_000;
/// Length of the fixed NTP header.
pub const NTP_PACKET_LEN: usize = 48;

const NS_PER_SEC: i128 = 1_000_000_000;
/// One NTP era (2^32 s) in nanoseconds.
const ERA_NS: i128 = (1i128 << 32) * NS_PER_SEC;
const OFFSET_NS: i128 = NTP_UNIX_OFFSET_SECS as i128 * NS_PER_SEC;

/// Errors from NTP packet handling and from querying a time source.
#[derive(Debug, thiserror::Error)]
pub enum NtpError {
    /// The datagram is shorter than the 48-byte NTP header.
    #[error("NTP packet too short: {0} bytes")]
    TooShort(usize),
    /// The reply mode is not 4 (server).
    #[error("unexpected NTP mode {0} (expected 4 = server)")]
    BadMode(u8),
    /// The reply version is neither 3 nor 4.
    #[error("unsupported NTP version {0}")]
    BadVersion(u8),
    /// The origin timestamp does not echo our request cookie (stale, duplicated or spoofed).
    #[error("origin timestamp does not match the request cookie")]
    CookieMismatch,
    /// Kiss-o'-Death (stratum 0); the 4-byte ASCII code, e.g. `RATE`, `DENY`, `RSTR`.
    #[error("kiss-o'-death from server: {}", kod_display(.0))]
    KissOfDeath([u8; 4]),
    /// The server reports leap indicator 3 (clock not synchronized).
    #[error("server clock is unsynchronized (LI=3)")]
    Unsynchronized,
    /// Stratum 16 or above (unsynchronized / invalid).
    #[error("invalid stratum {0}")]
    BadStratum(u8),
    /// The receive or transmit timestamp is zero.
    #[error("server timestamp is zero")]
    ZeroTimestamp,
    /// The server's root distance exceeds [`MAX_ROOT_DISTANCE_NS`].
    #[error("server root distance too large: {0} ns")]
    RootDistanceTooLarge(i64),
    /// The server's transmit timestamp precedes its receive timestamp.
    #[error("server transmit timestamp precedes its receive timestamp")]
    TransmitBeforeReceive,
    /// No valid reply arrived before the timeout.
    #[error("timed out waiting for the NTP reply")]
    Timeout,
    /// The host name could not be resolved.
    #[error("could not resolve {0}")]
    Resolve(String),
    /// The local monotonic clock went backwards during the exchange.
    #[error("local monotonic clock went backwards during the exchange")]
    LocalClockBackwards,
    /// Socket error.
    #[error("network I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl NtpError {
    /// The Kiss-o'-Death code, if this is a KoD.
    pub fn kod_code(&self) -> Option<[u8; 4]> {
        match self {
            NtpError::KissOfDeath(code) => Some(*code),
            _ => None,
        }
    }
}

fn kod_display(code: &[u8; 4]) -> String {
    code.iter()
        .map(|&b| {
            if b.is_ascii_graphic() {
                (b as char).to_string()
            } else {
                format!("\\x{b:02x}")
            }
        })
        .collect()
}

/// A 64-bit NTP timestamp: seconds since 1900-01-01 (modulo 2^32) and a 2^-32 s fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct NtpTimestamp {
    /// Whole seconds within the NTP era.
    pub secs: u32,
    /// Fraction of a second in units of 2^-32 s.
    pub frac: u32,
}

impl NtpTimestamp {
    /// Convert to UTC POSIX nanoseconds, choosing the NTP era closest to `pivot_utc_ns`.
    ///
    /// Any pivot within ±68 years of the true time gives the right answer (this handles the
    /// 2036 era rollover). Saturates at the `i64` range.
    pub fn to_utc_ns(self, pivot_utc_ns: i64) -> i64 {
        let frac_ns = ((self.frac as u64 * 1_000_000_000 + (1 << 31)) >> 32) as i128;
        let t = self.secs as i128 * NS_PER_SEC + frac_ns; // NTP ns within the era
        let pivot_ntp = pivot_utc_ns as i128 + OFFSET_NS;
        let era = (pivot_ntp - t + ERA_NS / 2).div_euclid(ERA_NS);
        crate::linear::sat_i64(era * ERA_NS + t - OFFSET_NS)
    }

    /// Convert UTC POSIX nanoseconds to an NTP timestamp (the era number is dropped).
    pub fn from_utc_ns(utc_ns: i64) -> Self {
        let ntp_ns = (utc_ns as i128 + OFFSET_NS).rem_euclid(ERA_NS);
        let secs = (ntp_ns / NS_PER_SEC) as u32;
        let rem = ntp_ns % NS_PER_SEC; // 0..1e9
        let frac = (((rem << 32) + NS_PER_SEC / 2) / NS_PER_SEC) as u32; // < 2^32 for rem < 1e9
        Self { secs, frac }
    }

    /// The raw 64-bit wire value (`secs << 32 | frac`).
    pub fn to_u64(self) -> u64 {
        ((self.secs as u64) << 32) | self.frac as u64
    }

    /// From the raw 64-bit wire value.
    pub fn from_u64(v: u64) -> Self {
        Self {
            secs: (v >> 32) as u32,
            frac: v as u32,
        }
    }
}

impl fmt::Display for NtpTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}.{:08x}", self.secs, self.frac)
    }
}

/// Build a 48-byte SNTPv4 client request (LI=0, VN=4, Mode=3).
///
/// Every other field is zero, except the transmit timestamp, which carries `transmit_cookie`
/// (a random 64-bit value, not the real time). A valid server copies it into the reply's origin
/// timestamp, which [`parse_response`] checks.
pub fn build_request(transmit_cookie: u64) -> [u8; 48] {
    let mut p = [0u8; NTP_PACKET_LEN];
    p[0] = (4 << 3) | 3; // LI = 0, VN = 4, Mode = 3 (client)
    p[40..48].copy_from_slice(&transmit_cookie.to_be_bytes());
    p
}

/// A validated server reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerReply {
    /// Leap indicator (0..=2 after validation).
    pub leap: u8,
    /// Protocol version (3 or 4).
    pub version: u8,
    /// Stratum (1..=15 after validation).
    pub stratum: u8,
    /// Poll exponent advertised by the server.
    pub poll: i8,
    /// Server clock precision exponent (log2 seconds).
    pub precision: i8,
    /// Root delay in nanoseconds.
    pub root_delay_ns: i64,
    /// Root dispersion in nanoseconds.
    pub root_dispersion_ns: i64,
    /// Reference identifier.
    pub ref_id: [u8; 4],
    /// `t2`: when the server received the request (UTC ns).
    pub receive_utc_ns: i64,
    /// `t3`: when the server sent the reply (UTC ns).
    pub transmit_utc_ns: i64,
}

impl ServerReply {
    /// `root_delay/2 + root_dispersion`, in nanoseconds.
    pub fn root_distance_ns(&self) -> i64 {
        root_distance(self.root_delay_ns, self.root_dispersion_ns)
    }
}

fn root_distance(delay: i64, dispersion: i64) -> i64 {
    (delay / 2).saturating_add(dispersion)
}

fn be_u32(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_be_bytes(a)
}

fn be_u64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(a)
}

/// NTP short format (16.16 seconds) to nanoseconds.
fn short_to_ns(v: u32) -> i64 {
    ((v as u64 * 1_000_000_000) >> 16) as i64
}

/// Parse and validate a server reply to the request built with `expected_cookie`.
///
/// Rejected, in this order:
/// - shorter than 48 bytes ([`NtpError::TooShort`]); extension fields after the header are ignored;
/// - mode ≠ 4 ([`NtpError::BadMode`]) or version not 3/4 ([`NtpError::BadVersion`]);
/// - origin timestamp ≠ cookie ([`NtpError::CookieMismatch`]), checked **before** Kiss-o'-Death so
///   an off-path attacker cannot forge a KoD to disable a source;
/// - stratum 0: [`NtpError::KissOfDeath`] with the reference id as the code;
/// - zero receive/transmit timestamp, LI = 3, stratum ≥ 16;
/// - root distance (`root_delay/2 + root_dispersion`) > 1.5 s;
/// - `t3 < t2`.
///
/// `pivot_utc_ns` is any rough current time (±68 years) used to pick the NTP era.
pub fn parse_response(
    buf: &[u8],
    expected_cookie: u64,
    pivot_utc_ns: i64,
) -> Result<ServerReply, NtpError> {
    if buf.len() < NTP_PACKET_LEN {
        return Err(NtpError::TooShort(buf.len()));
    }
    let leap = buf[0] >> 6;
    let version = (buf[0] >> 3) & 0x7;
    let mode = buf[0] & 0x7;
    if mode != 4 {
        return Err(NtpError::BadMode(mode));
    }
    if version != 3 && version != 4 {
        return Err(NtpError::BadVersion(version));
    }
    if be_u64(buf, 24) != expected_cookie {
        return Err(NtpError::CookieMismatch);
    }
    let stratum = buf[1];
    let mut ref_id = [0u8; 4];
    ref_id.copy_from_slice(&buf[12..16]);
    if stratum == 0 {
        return Err(NtpError::KissOfDeath(ref_id));
    }
    let receive_raw = be_u64(buf, 32);
    let transmit_raw = be_u64(buf, 40);
    if transmit_raw == 0 || receive_raw == 0 {
        return Err(NtpError::ZeroTimestamp);
    }
    if leap == 3 {
        return Err(NtpError::Unsynchronized);
    }
    if stratum >= 16 {
        return Err(NtpError::BadStratum(stratum));
    }
    let root_delay_ns = short_to_ns(be_u32(buf, 4));
    let root_dispersion_ns = short_to_ns(be_u32(buf, 8));
    let dist = root_distance(root_delay_ns, root_dispersion_ns);
    if dist > MAX_ROOT_DISTANCE_NS {
        return Err(NtpError::RootDistanceTooLarge(dist));
    }
    let receive_utc_ns = NtpTimestamp::from_u64(receive_raw).to_utc_ns(pivot_utc_ns);
    let transmit_utc_ns = NtpTimestamp::from_u64(transmit_raw).to_utc_ns(pivot_utc_ns);
    if transmit_utc_ns < receive_utc_ns {
        return Err(NtpError::TransmitBeforeReceive);
    }
    Ok(ServerReply {
        leap,
        version,
        stratum,
        poll: buf[2] as i8,
        precision: buf[3] as i8,
        root_delay_ns,
        root_dispersion_ns,
        ref_id,
        receive_utc_ns,
        transmit_utc_ns,
    })
}

/// Build a server reply packet (tests).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_reply(
    leap: u8,
    version: u8,
    mode: u8,
    stratum: u8,
    root_delay_short: u32,
    root_disp_short: u32,
    ref_id: [u8; 4],
    origin: u64,
    receive: u64,
    transmit: u64,
) -> [u8; 48] {
    let mut p = [0u8; NTP_PACKET_LEN];
    p[0] = ((leap & 3) << 6) | ((version & 7) << 3) | (mode & 7);
    p[1] = stratum;
    p[2] = 6;
    p[3] = (-24i8) as u8;
    p[4..8].copy_from_slice(&root_delay_short.to_be_bytes());
    p[8..12].copy_from_slice(&root_disp_short.to_be_bytes());
    p[12..16].copy_from_slice(&ref_id);
    p[24..32].copy_from_slice(&origin.to_be_bytes());
    p[32..40].copy_from_slice(&receive.to_be_bytes());
    p[40..48].copy_from_slice(&transmit.to_be_bytes());
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand::{Rng, SeedableRng};

    const SEC: i64 = 1_000_000_000;
    /// 2036-02-07T06:28:16Z: NTP era 0 -> era 1 rollover.
    const ROLLOVER_UTC_NS: i64 = ((1i64 << 32) - NTP_UNIX_OFFSET_SECS) * SEC;
    const PIVOT_2026: i64 = 1_767_225_600 * SEC;

    fn good_reply(cookie: u64, t2: i64, t3: i64) -> [u8; 48] {
        build_reply(
            0,
            4,
            4,
            1,
            0x0000_0010, // ~244 µs
            0x0000_0020, // ~488 µs
            *b"ONBR",
            cookie,
            NtpTimestamp::from_utc_ns(t2).to_u64(),
            NtpTimestamp::from_utc_ns(t3).to_u64(),
        )
    }

    #[test]
    fn request_layout() {
        let cookie = 0x0123_4567_89ab_cdefu64;
        let p = build_request(cookie);
        assert_eq!(p.len(), 48);
        assert_eq!(p[0], 0x23, "LI=0 VN=4 Mode=3");
        assert_eq!(p[0] >> 6, 0);
        assert_eq!((p[0] >> 3) & 7, 4);
        assert_eq!(p[0] & 7, 3);
        assert!(p[1..40].iter().all(|&b| b == 0), "no real time leaks");
        assert_eq!(&p[40..48], &cookie.to_be_bytes());
    }

    #[test]
    fn era_pivot_handles_2036() {
        let after = ROLLOVER_UTC_NS + SEC + 250_000_000;
        let ts = NtpTimestamp::from_utc_ns(after);
        assert_eq!(ts.secs, 1, "era 1 restarts at zero");
        assert_eq!(ts.to_utc_ns(PIVOT_2026), after);
        assert_eq!(ts.to_utc_ns(ROLLOVER_UTC_NS), after);
        let before = ROLLOVER_UTC_NS - SEC;
        let ts = NtpTimestamp::from_utc_ns(before);
        assert_eq!(ts.secs, u32::MAX);
        assert_eq!(ts.to_utc_ns(ROLLOVER_UTC_NS + 3600 * SEC), before);
        // Pivot a decade away still resolves correctly; the naive era-0 reading is 1900-ish.
        let pivot_2046 = PIVOT_2026 + 20 * 365 * 86_400 * SEC;
        assert_eq!(
            NtpTimestamp::from_utc_ns(after).to_utc_ns(pivot_2046),
            after
        );
        assert!(NtpTimestamp::from_utc_ns(after).to_utc_ns(-NTP_UNIX_OFFSET_SECS * SEC) < 0);
    }

    #[test]
    fn round_trip_sub_microsecond() {
        let mut rng = SmallRng::seed_from_u64(7);
        for _ in 0..20_000 {
            // 1970 .. 2200
            let utc: i64 = rng.gen_range(0..7_258_118_400 * SEC);
            let pivot = utc + rng.gen_range(-60 * 365 * 86_400 * SEC..60 * 365 * 86_400 * SEC);
            let back = NtpTimestamp::from_utc_ns(utc).to_utc_ns(pivot);
            assert!((back - utc).abs() <= 1, "utc {utc} back {back}");
        }
        // Negative UTC (pre-1970) and the extremes never panic.
        for &utc in &[i64::MIN, -1, 0, i64::MAX] {
            let _ = NtpTimestamp::from_utc_ns(utc).to_utc_ns(utc);
        }
        assert_eq!(
            NtpTimestamp { secs: 0, frac: 0 }.to_utc_ns(i64::MIN),
            i64::MIN
        );
        let ts = NtpTimestamp::from_u64(0xdead_beef_0000_0001);
        assert_eq!(NtpTimestamp::from_u64(ts.to_u64()), ts);
        assert_eq!(ts.to_string(), "deadbeef.00000001");
    }

    #[test]
    fn parses_good_reply() {
        let t2 = PIVOT_2026 + 123_456_789;
        let t3 = t2 + 40_000;
        let r = parse_response(&good_reply(99, t2, t3), 99, PIVOT_2026).unwrap();
        assert_eq!(r.stratum, 1);
        assert_eq!(r.version, 4);
        assert_eq!(r.leap, 0);
        assert_eq!(r.ref_id, *b"ONBR");
        assert_eq!(r.precision, -24);
        assert!((r.receive_utc_ns - t2).abs() <= 1);
        assert!((r.transmit_utc_ns - t3).abs() <= 1);
        assert_eq!(r.root_delay_ns, 244_140);
        assert_eq!(r.root_dispersion_ns, 488_281);
        assert_eq!(r.root_distance_ns(), 122_070 + 488_281);
        // Extension fields / trailing bytes are ignored.
        let mut long = good_reply(99, t2, t3).to_vec();
        long.extend_from_slice(&[0u8; 20]);
        assert!(parse_response(&long, 99, PIVOT_2026).is_ok());
        // Version 3 is accepted.
        let mut v3 = good_reply(99, t2, t3);
        v3[0] = (3 << 3) | 4;
        assert_eq!(parse_response(&v3, 99, PIVOT_2026).unwrap().version, 3);
    }

    #[test]
    fn rejects_bad_replies() {
        let t2 = PIVOT_2026;
        let t3 = t2 + 1000;
        let good = good_reply(5, t2, t3);
        let parse = |p: &[u8]| parse_response(p, 5, PIVOT_2026);

        assert!(matches!(parse(&good[..47]), Err(NtpError::TooShort(47))));
        assert!(matches!(parse(&[]), Err(NtpError::TooShort(0))));

        let mut p = good;
        p[0] = (4 << 3) | 3; // mode 3 (client)
        assert!(matches!(parse(&p), Err(NtpError::BadMode(3))));
        p[0] = (4 << 3) | 5; // broadcast
        assert!(matches!(parse(&p), Err(NtpError::BadMode(5))));

        let mut p = good;
        p[0] = (2 << 3) | 4;
        assert!(matches!(parse(&p), Err(NtpError::BadVersion(2))));

        assert!(matches!(
            parse_response(&good, 6, PIVOT_2026),
            Err(NtpError::CookieMismatch)
        ));

        let mut p = good;
        p[0] = (3 << 6) | (4 << 3) | 4; // LI = 3
        assert!(matches!(parse(&p), Err(NtpError::Unsynchronized)));

        let mut p = good;
        p[1] = 16;
        assert!(matches!(parse(&p), Err(NtpError::BadStratum(16))));

        let mut p = good;
        p[40..48].fill(0);
        assert!(matches!(parse(&p), Err(NtpError::ZeroTimestamp)));

        let mut p = good;
        p[4..8].copy_from_slice(&(2u32 << 16).to_be_bytes()); // root delay 2 s
        p[8..12].copy_from_slice(&(3u32 << 14).to_be_bytes()); // dispersion 0.75 s
        assert!(matches!(
            parse(&p),
            Err(NtpError::RootDistanceTooLarge(1_500_000_000..))
        ));

        let back = good_reply(5, t3, t2);
        assert!(matches!(parse(&back), Err(NtpError::TransmitBeforeReceive)));
    }

    #[test]
    fn kiss_of_death() {
        for code in [*b"RATE", *b"DENY", *b"RSTR"] {
            // Typical KoD: LI=3, stratum 0, ref id = code, timestamps possibly zero.
            let p = build_reply(3, 4, 4, 0, 0, 0, code, 77, 0, 0);
            let err = parse_response(&p, 77, PIVOT_2026).unwrap_err();
            assert_eq!(err.kod_code(), Some(code));
            assert!(err
                .to_string()
                .contains(std::str::from_utf8(&code).unwrap()));
            // A KoD that does not echo our cookie is ignored as spoofed.
            assert!(matches!(
                parse_response(&p, 78, PIVOT_2026),
                Err(NtpError::CookieMismatch)
            ));
        }
        let weird = NtpError::KissOfDeath([b'X', 0, 0xff, b'Y']);
        assert_eq!(weird.to_string(), "kiss-o'-death from server: X\\x00\\xffY");
        assert_eq!(NtpError::Timeout.kod_code(), None);
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut rng = SmallRng::seed_from_u64(1);
        let mut accepted = 0;
        for i in 0..20_000 {
            let len = rng.gen_range(0..64);
            let mut buf: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
            if i % 2 == 0 && buf.len() >= 48 {
                buf[0] = (buf[0] & 0xc0) | (4 << 3) | 4;
                buf[24..32].copy_from_slice(&42u64.to_be_bytes());
            }
            if parse_response(&buf, 42, rng.gen()).is_ok() {
                accepted += 1;
            }
        }
        assert!(accepted < 20_000);
    }
}
