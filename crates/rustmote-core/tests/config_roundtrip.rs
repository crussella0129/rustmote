//! Integration test for spec §7.2: write, read, verify equality.
//!
//! Exercises the full on-disk TOML shape with every section populated so a
//! future schema change cannot silently break the roundtrip.

use std::net::IpAddr;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use rustmote_core::config::{Config, CONFIG_FILE_NAME, CREDENTIALS_FILE_NAME};
use rustmote_core::credentials::CredentialMode;
use rustmote_core::registry::RemoteServer;
use rustmote_core::target::Target;

fn scratch_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "rustmote-it-{}-{}-{}",
        tag,
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
    ));
    p.push("config.toml");
    p
}

#[test]
fn populated_config_roundtrips_exactly() {
    let path = scratch_path("populated");

    let mut cfg = Config::default();
    cfg.general.credential_mode = CredentialMode::Keychain;
    cfg.general.default_server = Some("zima-brain".into());
    cfg.general.viewer_path = "/opt/rustdesk/rustdesk".into();

    let mut zima = RemoteServer::new(
        "zima-brain",
        "10.0.0.1".parse::<IpAddr>().unwrap(),
        "charles",
        22,
        21_116,
    );
    // Pin created_at and last_used to fixed values so the compared blobs are
    // bit-identical across the roundtrip.
    zima.created_at = Utc.with_ymd_and_hms(2026, 4, 1, 12, 0, 0).unwrap();
    zima.last_used = Some(Utc.with_ymd_and_hms(2026, 4, 19, 3, 14, 15).unwrap());
    zima.relay_key = Some("ssh-ed25519 AAAAExample".into());
    cfg.add_server(zima).unwrap();

    cfg.add_server(RemoteServer::new(
        "jetmother",
        "10.0.0.2".parse::<IpAddr>().unwrap(),
        "charles",
        2222,
        21_116,
    ))
    .unwrap();

    cfg.add_target(Target {
        id: "123456789".into(),
        ip: Some("10.0.0.42".parse().unwrap()),
        label: Some("voron-controller".into()),
        via_server: Some("zima-brain".into()),
        last_seen: Some(Utc.with_ymd_and_hms(2026, 4, 15, 9, 30, 0).unwrap()),
    })
    .unwrap();
    cfg.add_target(Target::new("987654321")).unwrap();

    cfg.save_to(&path).unwrap();
    let reloaded = Config::load_from(&path).unwrap();
    assert_eq!(cfg, reloaded);

    // And a second roundtrip must be idempotent (bytes-stable on re-save).
    reloaded.save_to(&path).unwrap();
    let reloaded_again = Config::load_from(&path).unwrap();
    assert_eq!(reloaded, reloaded_again);

    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn missing_file_loads_as_default() {
    let path = scratch_path("missing");
    assert!(!path.exists());
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(cfg, Config::default());
}

#[test]
fn file_names_match_spec() {
    assert_eq!(CONFIG_FILE_NAME, "config.toml");
    assert_eq!(CREDENTIALS_FILE_NAME, "credentials.toml");
}
