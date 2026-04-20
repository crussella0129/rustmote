//! Integration smoke test for `discovery` against a localhost-scoped CIDR.
//!
//! Exercises the full `Discovery::scan` orchestration — mDNS daemon boot,
//! ICMP client construct, ARP read, merge — but targets `127.0.0.1/32` so
//! the sweep completes well inside the 10-second `/24` budget and does
//! not depend on the CI environment's LAN topology. This is the test
//! cited in `tasks.md` TASK-006 (spec §11 Phase 6).

use std::time::{Duration, Instant};

use ipnet::Ipv4Net;
use rustmote_core::discovery::{Discovery, DEFAULT_SCAN_TIMEOUT};

#[tokio::test]
async fn localhost_scan_respects_budget() {
    let cidr: Ipv4Net = "127.0.0.1/32".parse().unwrap();
    let budget = Duration::from_secs(3);
    let start = Instant::now();

    let hosts = Discovery::new()
        .with_cidr(cidr)
        .with_timeout(budget)
        .scan()
        .await
        .expect("scan with explicit CIDR must not error on auto-detect");

    // Overall scan must finish inside the budget + a small slack for
    // the tokio runtime shutdown.
    let elapsed = start.elapsed();
    assert!(
        elapsed <= budget + Duration::from_secs(1),
        "scan took {elapsed:?} — expected <= {:?}",
        budget + Duration::from_secs(1)
    );

    // Results may be empty (no mDNS responders, ICMP unprivileged,
    // empty /proc/net/arp) or contain entries from the host's ARP
    // cache. We only assert the shape — the important signal is that
    // the orchestration completed without panicking or hanging.
    for h in &hosts {
        assert!(h.ip.is_ipv4() || h.ip.is_ipv6());
    }
}

#[test]
fn default_timeout_is_within_spec_budget() {
    // Spec §3.6 mandates a /24 scan completes in < 10s.
    assert!(DEFAULT_SCAN_TIMEOUT < Duration::from_secs(10));
}
