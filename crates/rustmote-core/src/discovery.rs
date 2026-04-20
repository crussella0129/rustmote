//! Concurrent mDNS + ICMP ping + ARP LAN discovery (spec §3.6).
//!
//! Three independent sweeps run in parallel via `tokio::join!`:
//!
//! 1. **mDNS** via `mdns-sd` browsing `_ssh._tcp.local.` and
//!    `_workstation._tcp.local.` service types. The `ServiceDaemon`
//!    spawns its own thread and exposes `flume` channels — we consume
//!    the async receiver with a `timeout` wrapper so a quiet LAN does
//!    not hang the scan.
//! 2. **ICMP** via `surge-ping` across every host address in the
//!    configured `/24` (or other CIDR), fanned out with
//!    `futures::future::join_all`. Raw-socket permission errors are
//!    swallowed with a `tracing::warn!` — unprivileged environments
//!    (CI containers, sandbox) produce an empty ICMP list rather than
//!    a hard error.
//! 3. **ARP** via `/proc/net/arp` on Linux; other platforms return
//!    empty and log a one-time warning (spec §13 open question — Linux
//!    is the v0.1 target platform for LAN-level tooling).
//!
//! Results are merged by IP: a single `DiscoveredHost` is emitted per
//! unique address, with hostname (from mDNS) and MAC (from ARP) filled
//! in when available. `is_known_server` is true when the IP matches an
//! entry in the supplied known-servers list (callers derive this from
//! the registry). The overall deadline defaults to 9 seconds, inside
//! the spec's 10-second /24 budget.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::Duration;

use ipnet::Ipv4Net;

use crate::error::RustmoteError;

// -----------------------------------------------------------------------------
// Public data types
// -----------------------------------------------------------------------------

/// A host discovered on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    /// The IP address that identifies this host in the scan results.
    pub ip: IpAddr,
    /// Hostname if advertised over mDNS.
    pub hostname: Option<String>,
    /// MAC address (formatted `aa:bb:cc:dd:ee:ff`) if seen in the ARP
    /// table. Missing for hosts discovered only via mDNS or ICMP.
    pub mac: Option<String>,
    /// `true` when `ip` matches a `RemoteServer.host` in the registry.
    pub is_known_server: bool,
}

// -----------------------------------------------------------------------------
// Discovery configuration
// -----------------------------------------------------------------------------

/// Default overall scan timeout. Sits inside spec §3.6's 10-second /24
/// budget with a little headroom for the merge + known-server pass.
pub const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(9);

/// Configurable discovery driver.
#[derive(Debug, Clone)]
pub struct Discovery {
    cidr: Option<Ipv4Net>,
    timeout: Duration,
    known_servers: Vec<IpAddr>,
}

impl Default for Discovery {
    fn default() -> Self {
        Self::new()
    }
}

impl Discovery {
    /// Construct a discovery driver with default timeout and no CIDR
    /// override — [`Self::scan`] will auto-detect the local subnet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cidr: None,
            timeout: DEFAULT_SCAN_TIMEOUT,
            known_servers: Vec::new(),
        }
    }

    /// Scan this CIDR instead of the auto-detected local subnet.
    #[must_use]
    pub fn with_cidr(mut self, cidr: Ipv4Net) -> Self {
        self.cidr = Some(cidr);
        self
    }

    /// Override the overall scan timeout (default 9s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Mark discovered hosts whose IP matches any of `servers` as
    /// `is_known_server = true` in the results.
    #[must_use]
    pub fn with_known_servers(mut self, servers: Vec<IpAddr>) -> Self {
        self.known_servers = servers;
        self
    }

    /// Run the three sweeps concurrently and return merged results.
    ///
    /// # Errors
    /// [`RustmoteError::DiscoveryNoInterface`] when no CIDR override
    /// was supplied and no usable IPv4 interface can be detected.
    pub async fn scan(&self) -> crate::Result<Vec<DiscoveredHost>> {
        let cidr = match self.cidr {
            Some(c) => c,
            None => auto_detect_cidr()?,
        };

        let overall = self.timeout;
        // Give each sweep most of the budget. The merge pass is
        // O(hosts) with no I/O, so we don't reserve time for it.
        let sweep_budget = overall;

        let (mdns_raw, icmp_raw, arp_raw) = tokio::join!(
            run_with_budget(sweep_budget, mdns_sweep(sweep_budget)),
            run_with_budget(sweep_budget, icmp_sweep(cidr, sweep_budget)),
            run_with_budget(sweep_budget, arp_read()),
        );

        Ok(merge_results(
            mdns_raw.unwrap_or_default(),
            icmp_raw.unwrap_or_default(),
            arp_raw.unwrap_or_default(),
            &self.known_servers,
        ))
    }
}

// -----------------------------------------------------------------------------
// Budget wrapper — per-sweep soft timeout
// -----------------------------------------------------------------------------

async fn run_with_budget<T: Default>(
    budget: Duration,
    fut: impl std::future::Future<Output = T>,
) -> Option<T> {
    if let Ok(v) = tokio::time::timeout(budget, fut).await {
        Some(v)
    } else {
        tracing::warn!(?budget, "discovery sweep hit overall timeout");
        None
    }
}

// -----------------------------------------------------------------------------
// mDNS sweep
// -----------------------------------------------------------------------------

/// What a single mDNS service resolution yields.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MdnsFinding {
    ip: IpAddr,
    hostname: Option<String>,
}

const MDNS_SERVICE_TYPES: &[&str] = &["_ssh._tcp.local.", "_workstation._tcp.local."];

async fn mdns_sweep(budget: Duration) -> Vec<MdnsFinding> {
    let daemon = match mdns_sd::ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "mDNS daemon could not start; skipping mDNS sweep");
            return Vec::new();
        }
    };

    let mut findings = Vec::new();
    let mut receivers = Vec::new();
    for service in MDNS_SERVICE_TYPES {
        match daemon.browse(service) {
            Ok(rx) => receivers.push(rx),
            Err(e) => {
                tracing::warn!(error = %e, service = %service, "mDNS browse failed");
            }
        }
    }

    // Collect events until the budget expires. Each receiver yields
    // independently; drain them in a round-robin under a single
    // deadline rather than sequencing them.
    let deadline = tokio::time::Instant::now() + budget;
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let mut progressed = false;
        for rx in &receivers {
            if let Ok(Ok(event)) =
                tokio::time::timeout(remaining.min(Duration::from_millis(200)), rx.recv_async())
                    .await
            {
                progressed = true;
                if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                    let hostname = info.get_hostname();
                    let hostname = if hostname.is_empty() {
                        None
                    } else {
                        Some(hostname.trim_end_matches('.').to_owned())
                    };
                    for addr in info.get_addresses() {
                        findings.push(MdnsFinding {
                            ip: *addr,
                            hostname: hostname.clone(),
                        });
                    }
                }
            }
        }
        if !progressed {
            // Nothing arrived in a full round — wait a bit and retry
            // until the overall budget expires.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Stop the daemon so its thread exits. Errors are non-fatal — we
    // already have the results we need.
    if let Err(e) = daemon.shutdown() {
        tracing::debug!(error = ?e, "mDNS daemon shutdown returned error (ignored)");
    }

    findings
}

// -----------------------------------------------------------------------------
// ICMP sweep
// -----------------------------------------------------------------------------

async fn icmp_sweep(cidr: Ipv4Net, budget: Duration) -> Vec<IpAddr> {
    // Per-host ping budget — enough that a reachable host replies,
    // short enough that 254 parallel probes fit in the overall budget.
    let per_host = Duration::from_millis(800).min(budget / 4);

    let client = match surge_ping::Client::new(&surge_ping::Config::default()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "ICMP client construct failed (missing CAP_NET_RAW?); skipping ICMP sweep");
            return Vec::new();
        }
    };

    let hosts: Vec<Ipv4Addr> = cidr.hosts().collect();
    let futures = hosts.into_iter().map({
        let client = client.clone();
        move |ip| {
            let client = client.clone();
            async move {
                let id = surge_ping::PingIdentifier(rand_u16());
                let mut pinger = client.pinger(IpAddr::V4(ip), id).await;
                pinger.timeout(per_host);
                match pinger.ping(surge_ping::PingSequence(0), &[0u8; 8]).await {
                    Ok(_) => Some(IpAddr::V4(ip)),
                    Err(_) => None,
                }
            }
        }
    });
    futures::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect()
}

fn rand_u16() -> u16 {
    use std::hash::{BuildHasher, Hasher, RandomState};
    // Avoid pulling in `rand` just for a ping identifier — the stdlib
    // `RandomState` is seeded from the OS and gives us a 64-bit hash we
    // XOR-fold into 16 bits. Collisions only matter within one scan,
    // which is vanishingly unlikely across 254 probes.
    let mut h = RandomState::new().build_hasher();
    let nanos: u128 = std::time::SystemTime::now()
        .elapsed()
        .map_or(0, |d| d.as_nanos());
    #[allow(clippy::cast_possible_truncation)]
    let nanos_low = nanos as u64;
    h.write_u64(nanos_low);
    let full = h.finish();
    #[allow(clippy::cast_possible_truncation)]
    let folded = ((full ^ (full >> 16) ^ (full >> 32) ^ (full >> 48)) & 0xFFFF) as u16;
    folded
}

// -----------------------------------------------------------------------------
// ARP read
// -----------------------------------------------------------------------------

const LINUX_ARP_TABLE: &str = "/proc/net/arp";

async fn arp_read() -> Vec<(IpAddr, String)> {
    arp_read_from(Path::new(LINUX_ARP_TABLE)).await
}

async fn arp_read_from(path: &Path) -> Vec<(IpAddr, String)> {
    // `/proc/net/arp` is a Linux-only kernel file. On other platforms
    // the read fails and we return empty — the pure parser below is
    // still compiled everywhere so tests that cover it stay portable.
    match tokio::fs::read_to_string(path).await {
        Ok(body) => parse_proc_net_arp(&body),
        Err(e) => {
            tracing::debug!(error = %e, "arp: could not read {}; skipping", path.display());
            Vec::new()
        }
    }
}

fn parse_proc_net_arp(body: &str) -> Vec<(IpAddr, String)> {
    // IP address       HW type     Flags       HW address            Mask     Device
    // 192.168.1.1     0x1         0x2         ab:cd:ef:12:34:56     *        eth0
    let mut out = Vec::new();
    for line in body.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let Ok(ip) = cols[0].parse::<IpAddr>() else {
            continue;
        };
        let mac = cols[3];
        if mac == "00:00:00:00:00:00" {
            // Incomplete entry — kernel has the IP but no resolved MAC yet.
            continue;
        }
        if !looks_like_mac(mac) {
            continue;
        }
        out.push((ip, mac.to_owned()));
    }
    out
}

fn looks_like_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

// -----------------------------------------------------------------------------
// CIDR auto-detection
// -----------------------------------------------------------------------------

fn auto_detect_cidr() -> crate::Result<Ipv4Net> {
    let ifaces = if_addrs::get_if_addrs().map_err(|_| RustmoteError::DiscoveryNoInterface)?;
    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            if let Ok(net) = Ipv4Net::new(v4.ip, v4.prefixlen) {
                return Ok(net.trunc());
            }
        }
    }
    Err(RustmoteError::DiscoveryNoInterface)
}

// -----------------------------------------------------------------------------
// Merge
// -----------------------------------------------------------------------------

fn merge_results(
    mdns: Vec<MdnsFinding>,
    icmp: Vec<IpAddr>,
    arp: Vec<(IpAddr, String)>,
    known_servers: &[IpAddr],
) -> Vec<DiscoveredHost> {
    let known: HashSet<IpAddr> = known_servers.iter().copied().collect();
    let mut by_ip: BTreeMap<IpAddr, DiscoveredHost> = BTreeMap::new();

    for finding in mdns {
        let host = by_ip.entry(finding.ip).or_insert_with(|| DiscoveredHost {
            ip: finding.ip,
            hostname: None,
            mac: None,
            is_known_server: known.contains(&finding.ip),
        });
        if host.hostname.is_none() {
            host.hostname = finding.hostname;
        }
    }
    for ip in icmp {
        by_ip.entry(ip).or_insert_with(|| DiscoveredHost {
            ip,
            hostname: None,
            mac: None,
            is_known_server: known.contains(&ip),
        });
    }
    for (ip, mac) in arp {
        let host = by_ip.entry(ip).or_insert_with(|| DiscoveredHost {
            ip,
            hostname: None,
            mac: None,
            is_known_server: known.contains(&ip),
        });
        if host.mac.is_none() {
            host.mac = Some(mac);
        }
    }

    by_ip.into_values().collect()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn merge_dedups_by_ip_across_sources() {
        let mdns = vec![MdnsFinding {
            ip: ip("192.168.1.5"),
            hostname: Some("alpha.local".into()),
        }];
        let icmp = vec![ip("192.168.1.5"), ip("192.168.1.7")];
        let arp = vec![
            (ip("192.168.1.5"), "aa:bb:cc:dd:ee:01".into()),
            (ip("192.168.1.9"), "aa:bb:cc:dd:ee:02".into()),
        ];

        let merged = merge_results(mdns, icmp, arp, &[]);

        // .5 is in all three; .7 ICMP-only; .9 ARP-only → three hosts total.
        assert_eq!(merged.len(), 3);

        let five = merged.iter().find(|h| h.ip == ip("192.168.1.5")).unwrap();
        assert_eq!(five.hostname.as_deref(), Some("alpha.local"));
        assert_eq!(five.mac.as_deref(), Some("aa:bb:cc:dd:ee:01"));

        let seven = merged.iter().find(|h| h.ip == ip("192.168.1.7")).unwrap();
        assert!(seven.hostname.is_none() && seven.mac.is_none());

        let nine = merged.iter().find(|h| h.ip == ip("192.168.1.9")).unwrap();
        assert!(nine.hostname.is_none());
        assert_eq!(nine.mac.as_deref(), Some("aa:bb:cc:dd:ee:02"));
    }

    #[test]
    fn merge_marks_known_servers() {
        let mdns = vec![];
        let icmp = vec![ip("10.0.0.1"), ip("10.0.0.2")];
        let arp = vec![];
        let known = vec![ip("10.0.0.2")];

        let merged = merge_results(mdns, icmp, arp, &known);
        let one = merged.iter().find(|h| h.ip == ip("10.0.0.1")).unwrap();
        let two = merged.iter().find(|h| h.ip == ip("10.0.0.2")).unwrap();
        assert!(!one.is_known_server);
        assert!(two.is_known_server);
    }

    #[test]
    fn merge_preserves_first_hostname_when_duplicated() {
        // Two mDNS findings for the same IP — first hostname wins.
        let mdns = vec![
            MdnsFinding {
                ip: ip("192.168.1.5"),
                hostname: Some("first.local".into()),
            },
            MdnsFinding {
                ip: ip("192.168.1.5"),
                hostname: Some("second.local".into()),
            },
        ];
        let merged = merge_results(mdns, vec![], vec![], &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].hostname.as_deref(), Some("first.local"));
    }

    #[test]
    fn parse_arp_skips_header_and_incomplete() {
        let body = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.1.1     0x1         0x2         ab:cd:ef:12:34:56     *        eth0
192.168.1.99    0x1         0x0         00:00:00:00:00:00     *        eth0
10.0.0.5        0x1         0x2         01:23:45:67:89:ab     *        wlan0
";
        let parsed = parse_proc_net_arp(body);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, ip("192.168.1.1"));
        assert_eq!(parsed[0].1, "ab:cd:ef:12:34:56");
        assert_eq!(parsed[1].0, ip("10.0.0.5"));
    }

    #[test]
    fn parse_arp_rejects_malformed_mac() {
        let body = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.1.1     0x1         0x2         nothex:xx:xx:xx:xx:xx *        eth0
192.168.1.2     0x1         0x2         aa:bb:cc:dd:ee         *        eth0
";
        assert!(parse_proc_net_arp(body).is_empty());
    }

    #[test]
    fn looks_like_mac_boundaries() {
        assert!(looks_like_mac("00:11:22:33:44:55"));
        assert!(looks_like_mac("AA:BB:CC:DD:EE:FF"));
        assert!(!looks_like_mac(""));
        assert!(!looks_like_mac("00:11:22:33:44"));
        assert!(!looks_like_mac("00:11:22:33:44:55:66"));
        assert!(!looks_like_mac("GG:11:22:33:44:55"));
    }

    #[tokio::test]
    async fn scan_against_empty_cidr_completes_fast() {
        // /32 has zero host addresses in ipnet — the three sweeps
        // short-circuit and the overall scan returns quickly. Proves
        // the concurrent orchestration doesn't deadlock on an empty
        // CIDR and respects the budget wrapper.
        let cidr: Ipv4Net = "127.0.0.1/32".parse().unwrap();
        let start = std::time::Instant::now();
        let hosts = Discovery::new()
            .with_cidr(cidr)
            .with_timeout(Duration::from_secs(2))
            .scan()
            .await
            .unwrap();
        assert!(start.elapsed() <= Duration::from_secs(3));
        // Hosts may be non-empty if /proc/net/arp or mDNS had entries —
        // we only assert the call completed inside budget.
        for h in &hosts {
            assert!(h.ip.is_ipv4() || h.ip.is_ipv6());
        }
    }
}
