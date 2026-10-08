//! Blocking SNTP client over `std::net::UdpSocket` (fallback until the NTS client exists).
//!
//! Each query uses a fresh ephemeral socket, `connect`ed to the server so the OS drops datagrams
//! from other addresses, and a fresh random 64-bit cookie in the transmit timestamp. Replies
//! that do not echo the cookie (late duplicates, spoofing attempts) are ignored until the
//! timeout. `t1` and `t4` are read from the injected [`MonotonicClock`], never from the system
//! clock. Polling policy (how often to call [`TimeSource::query`]) lives in
//! [`crate::PollScheduler`].

use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::clock_source::MonotonicClock;
use crate::ntp::{build_request, parse_response, NtpError};
use crate::sample::Sample;

/// Default servers (docs 8.3): NTP.br (stratum 1, cesium clocks of the Observatório Nacional)
/// and Cloudflare (anycast, no leap smear). Google/AWS are excluded because they smear leap
/// seconds; `pool.ntp.org` must not be embedded in apps.
pub const DEFAULT_SERVERS: &[&str] = &[
    "a.st1.ntp.br",
    "b.st1.ntp.br",
    "c.st1.ntp.br",
    "d.st1.ntp.br",
    "e.st1.ntp.br",
    "time.cloudflare.com",
];

/// Default NTP port.
pub const NTP_PORT: u16 = 123;
/// Default reply timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// Default era pivot: 2026-01-01T00:00:00Z (valid for NTP timestamps until about 2094).
pub const DEFAULT_PIVOT_UTC_NS: i64 = 1_767_225_600 * 1_000_000_000;

/// A source of time samples.
pub trait TimeSource: Send {
    /// Display / identification name.
    fn name(&self) -> &str;
    /// Whether replies are cryptographically authenticated (false for SNTP; NTS later).
    fn authenticated(&self) -> bool;
    /// Perform one exchange, timing it with `clock`.
    fn query(&mut self, clock: &dyn MonotonicClock) -> Result<Sample, NtpError>;
}

/// An unauthenticated SNTPv4 source.
#[derive(Debug)]
pub struct SntpSource {
    name: String,
    host: String,
    port: u16,
    addr: Option<SocketAddr>,
    timeout: Duration,
    pivot_utc_ns: i64,
    rng: StdRng,
}

impl SntpSource {
    /// A source for `"host"`, `"host:port"`, `"1.2.3.4:123"` or `"[::1]:123"`.
    /// Resolution is lazy (on the first query) and repeated after socket errors.
    pub fn new(server: &str) -> Self {
        let (host, port) = split_host_port(server);
        Self {
            name: server.to_string(),
            host,
            port,
            addr: None,
            timeout: DEFAULT_TIMEOUT,
            pivot_utc_ns: DEFAULT_PIVOT_UTC_NS,
            rng: StdRng::from_entropy(),
        }
    }

    /// All [`DEFAULT_SERVERS`].
    pub fn defaults() -> Vec<SntpSource> {
        DEFAULT_SERVERS.iter().map(|s| SntpSource::new(s)).collect()
    }

    /// Set the reply timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Use a deterministic cookie generator (tests).
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = StdRng::seed_from_u64(seed);
        self
    }

    /// Set the rough current UTC used to resolve the NTP era (any value within ±68 years).
    pub fn set_pivot_utc_ns(&mut self, pivot_utc_ns: i64) {
        self.pivot_utc_ns = pivot_utc_ns;
    }

    /// Host part of the server name.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Port.
    pub fn port(&self) -> u16 {
        self.port
    }

    fn resolve(&mut self) -> Result<SocketAddr, NtpError> {
        if let Some(a) = self.addr {
            return Ok(a);
        }
        let mut addrs = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| NtpError::Resolve(format!("{}: {e}", self.host)))?;
        let addr = addrs
            .next()
            .ok_or_else(|| NtpError::Resolve(self.host.clone()))?;
        self.addr = Some(addr);
        Ok(addr)
    }

    fn exchange(
        &mut self,
        addr: SocketAddr,
        clock: &dyn MonotonicClock,
    ) -> Result<Sample, NtpError> {
        let bind: SocketAddr = if addr.is_ipv4() {
            SocketAddr::from(([0u8; 4], 0))
        } else {
            SocketAddr::from(([0u16; 8], 0))
        };
        let socket = UdpSocket::bind(bind)?;
        socket.connect(addr)?;
        let cookie = loop {
            let c: u64 = self.rng.gen();
            if c != 0 {
                break c;
            }
        };
        let request = build_request(cookie);
        let deadline = Instant::now() + self.timeout;
        let t1 = clock.now_ns();
        socket.send(&request)?;
        let mut buf = [0u8; 512];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(NtpError::Timeout);
            }
            socket.set_read_timeout(Some(remaining))?;
            let n = match socket.recv(&mut buf) {
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(NtpError::Timeout)
                }
                Err(e) => return Err(NtpError::Io(e)),
            };
            let t4 = clock.now_ns();
            match parse_response(&buf[..n.min(buf.len())], cookie, self.pivot_utc_ns) {
                Ok(reply) => {
                    if t4 < t1 {
                        return Err(NtpError::LocalClockBackwards);
                    }
                    return Ok(Sample::from_server(
                        t1,
                        reply.receive_utc_ns,
                        reply.transmit_utc_ns,
                        t4,
                        reply.root_distance_ns(),
                    ));
                }
                // Not provably an answer to this request (garbage, late duplicate, spoofing):
                // keep waiting for the real one.
                Err(
                    NtpError::CookieMismatch
                    | NtpError::TooShort(_)
                    | NtpError::BadMode(_)
                    | NtpError::BadVersion(_),
                ) => continue,
                // The reply echoes our cookie, so its verdict (KoD, LI=3, ...) is authentic.
                Err(e) => return Err(e),
            }
        }
    }
}

impl TimeSource for SntpSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn authenticated(&self) -> bool {
        false
    }

    fn query(&mut self, clock: &dyn MonotonicClock) -> Result<Sample, NtpError> {
        let addr = self.resolve()?;
        let result = self.exchange(addr, clock);
        if matches!(result, Err(NtpError::Io(_)) | Err(NtpError::Timeout)) {
            // The address may have changed (anycast, DNS rotation, network change).
            self.addr = None;
        }
        result
    }
}

fn split_host_port(server: &str) -> (String, u16) {
    let s = server.trim();
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(NTP_PORT);
            return (host.to_string(), port);
        }
    }
    match s.rsplit_once(':') {
        // Exactly one ':' -> host:port. More than one -> bare IPv6 literal.
        Some((host, port)) if !host.contains(':') => match port.parse() {
            Ok(p) => (host.to_string(), p),
            Err(_) => (s.to_string(), NTP_PORT),
        },
        _ => (s.to_string(), NTP_PORT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock_source::StdMonotonic;
    use crate::ntp::{build_reply, NtpTimestamp};
    use std::thread;

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            split_host_port("a.st1.ntp.br"),
            ("a.st1.ntp.br".into(), 123)
        );
        assert_eq!(
            split_host_port("localhost:1234"),
            ("localhost".into(), 1234)
        );
        assert_eq!(split_host_port("[::1]:5"), ("::1".into(), 5));
        assert_eq!(split_host_port("[::1]"), ("::1".into(), 123));
        assert_eq!(split_host_port("fe80::1"), ("fe80::1".into(), 123));
        assert_eq!(split_host_port("host:abc"), ("host:abc".into(), 123));
        let s = SntpSource::new("time.cloudflare.com");
        assert_eq!(s.name(), "time.cloudflare.com");
        assert!(!s.authenticated());
        assert_eq!(SntpSource::defaults().len(), DEFAULT_SERVERS.len());
    }

    /// A local fake server: answers a garbage datagram and a wrong-cookie reply first (both
    /// must be ignored), then the real reply.
    #[test]
    fn query_against_local_fake_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            assert_eq!(n, 48);
            assert_eq!(buf[0], 0x23);
            let mut cookie = [0u8; 8];
            cookie.copy_from_slice(&buf[40..48]);
            let cookie = u64::from_be_bytes(cookie);
            let now = DEFAULT_PIVOT_UTC_NS + 42 * 1_000_000_000;
            let ts = NtpTimestamp::from_utc_ns(now).to_u64();
            server.send_to(&[1, 2, 3], from).unwrap();
            let wrong = build_reply(0, 4, 4, 1, 0, 0, *b"GPS\0", cookie ^ 1, ts, ts);
            server.send_to(&wrong, from).unwrap();
            let good = build_reply(0, 4, 4, 1, 1 << 8, 1 << 8, *b"GPS\0", cookie, ts, ts);
            server.send_to(&good, from).unwrap();
        });
        let clock = StdMonotonic::new();
        let mut src = SntpSource::new(&addr.to_string())
            .with_seed(3)
            .with_timeout(Duration::from_secs(5));
        let s = src.query(&clock).unwrap();
        handle.join().unwrap();
        assert!(s.delay_ns >= 0 && s.delay_ns < 1_000_000_000);
        assert_eq!(s.root_distance_ns, 3_906_250 / 2 + 3_906_250);
        let utc_at_t4 = s.at_local_ns as i128 + s.offset_ns as i128;
        let expected = DEFAULT_PIVOT_UTC_NS as i128 + 42_000_000_000;
        assert!((utc_at_t4 - expected).abs() < 1_000_000_000);
    }

    #[test]
    fn kod_and_timeout_from_local_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (_, from) = server.recv_from(&mut buf).unwrap();
            let mut cookie = [0u8; 8];
            cookie.copy_from_slice(&buf[40..48]);
            let kod = build_reply(3, 4, 4, 0, 0, 0, *b"RATE", u64::from_be_bytes(cookie), 0, 0);
            server.send_to(&kod, from).unwrap();
            // Second request: never answered.
            let _ = server.recv_from(&mut buf);
        });
        let clock = StdMonotonic::new();
        let mut src = SntpSource::new(&addr.to_string()).with_timeout(Duration::from_millis(300));
        let err = src.query(&clock).unwrap_err();
        assert_eq!(err.kod_code(), Some(*b"RATE"));
        let err = src.query(&clock).unwrap_err();
        assert!(matches!(err, NtpError::Timeout), "{err:?}");
        handle.join().unwrap();
    }

    /// Real network: UDP 123 may be blocked in CI/sandboxes.
    #[test]
    #[ignore]
    fn query_time_cloudflare_com() {
        let clock = StdMonotonic::new();
        let mut src = SntpSource::new("time.cloudflare.com");
        let s = src.query(&clock).expect("query time.cloudflare.com");
        assert!(s.delay_ns > 0 && s.delay_ns < 1_000_000_000);
        let utc = s.at_local_ns as i128 + s.offset_ns as i128;
        let sys = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i128;
        assert!(
            (utc - sys).abs() < 60_000_000_000,
            "system clock within a minute"
        );
    }
}
