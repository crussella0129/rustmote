//! CLI smoke tests per spec §7.3.
//!
//! Each test invokes the built `rustmote` binary through `assert_cmd`,
//! isolating config state via the `RUSTMOTE_CONFIG_DIR` env override so
//! tests never touch the developer's real config directory.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

/// Produce a unique scratch config directory for each test invocation.
fn scratch_config_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "rustmote-cli-test-{}-{}-{}",
        tag,
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
    ));
    p
}

#[test]
fn help_exits_zero_and_mentions_every_subcommand_group() {
    Command::cargo_bin("rustmote")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("server"))
        .stdout(predicate::str::contains("target"))
        .stdout(predicate::str::contains("connect"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("relay"))
        .stdout(predicate::str::contains("config"));
}

#[test]
fn server_list_on_empty_config_exits_zero() {
    let dir = scratch_config_dir("server-list-empty");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["server", "list"])
        .assert()
        .success();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_path_prints_path_under_config_dir() {
    let dir = scratch_config_dir("config-path");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains(dir.to_string_lossy().as_ref()))
        .stdout(predicate::str::contains("config.toml"));
    std::fs::remove_dir_all(&dir).ok();
}

// Spec §7.3 also requires:
//   rustmote relay check-updates nonexistent-server → UnknownServer, non-zero
// That handler lands in Phase 13 (TASK-013). The test is added there so
// it meaningfully exercises the registry-lookup failure path rather than
// a placeholder bail.

#[test]
fn server_add_list_show_roundtrip() {
    let dir = scratch_config_dir("server-roundtrip");
    let bin = || Command::cargo_bin("rustmote").unwrap();

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args([
            "server",
            "add",
            "zima-brain",
            "--host",
            "10.0.0.1",
            "--user",
            "charles",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("zima-brain"));

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["server", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zima-brain"))
        .stdout(predicate::str::contains("10.0.0.1"));

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["server", "show", "zima-brain", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"zima-brain\""))
        .stdout(predicate::str::contains("\"ssh_user\": \"charles\""));

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["server", "remove", "zima-brain"])
        .assert()
        .success();

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn server_add_rejects_invalid_name_per_spec_6_4() {
    let dir = scratch_config_dir("server-bad-name");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args([
            "server",
            "add",
            "bad name with space",
            "--host",
            "10.0.0.1",
            "--user",
            "u",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid server name"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn server_add_without_user_on_non_tty_fails_with_clear_message() {
    let dir = scratch_config_dir("server-no-user-no-tty");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["server", "add", "zima-brain", "--host", "10.0.0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--user"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn target_list_on_empty_config_exits_zero() {
    let dir = scratch_config_dir("target-list-empty");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["target", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no targets registered"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn target_add_list_remove_roundtrip() {
    let dir = scratch_config_dir("target-roundtrip");
    let bin = || Command::cargo_bin("rustmote").unwrap();

    // Register a server first so --via resolves.
    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args([
            "server",
            "add",
            "zima-brain",
            "--host",
            "10.0.0.1",
            "--user",
            "charles",
        ])
        .assert()
        .success();

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args([
            "target",
            "add",
            "123456789",
            "--label",
            "voron",
            "--via",
            "zima-brain",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("123456789"));

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["target", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"id\": \"123456789\""))
        .stdout(predicate::str::contains("\"label\": \"voron\""))
        .stdout(predicate::str::contains("\"via_server\": \"zima-brain\""));

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["target", "remove", "123456789"])
        .assert()
        .success();

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn target_add_rejects_invalid_id_per_spec_3_5() {
    let dir = scratch_config_dir("target-bad-id");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["target", "add", "not-a-number"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid RustDesk target id"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn connect_with_nothing_resolvable_fails_with_hint() {
    // No target registered, arg is not a valid RustDesk ID, no server
    // resolvable — the pre-session resolver should bail before any
    // network I/O happens. Confirms spec §4.1 "<target>" parsing stops
    // cold at a bad argument rather than racing to SSH.
    let dir = scratch_config_dir("connect-no-target");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["connect", "not-a-number"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not-a-number"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn connect_without_via_and_no_default_fails_cleanly() {
    // Valid RustDesk ID, but no --via, no target with via_server, no
    // default_server. Exercises the resolve_via_name error path.
    let dir = scratch_config_dir("connect-no-via");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["connect", "123456789"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--via"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn target_add_rejects_unknown_via_server() {
    let dir = scratch_config_dir("target-unknown-via");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["target", "add", "123456789", "--via", "nonexistent-server"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nonexistent-server"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_show_on_empty_exits_zero_with_defaults() {
    let dir = scratch_config_dir("config-show-empty");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("credential_mode"))
        .stdout(predicate::str::contains("prompt"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_show_json_is_parseable_and_has_expected_defaults() {
    let dir = scratch_config_dir("config-show-json");
    let assert = Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "show", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["general"]["credential_mode"], "prompt");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_set_mode_keychain_persists_and_is_visible_in_show() {
    let dir = scratch_config_dir("config-set-mode");
    let bin = || Command::cargo_bin("rustmote").unwrap();

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "set-mode", "keychain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("keychain"));

    bin()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("keychain"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_set_mode_unsafe_without_ack_is_refused() {
    let dir = scratch_config_dir("config-set-mode-unsafe-no-ack");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "set-mode", "unsafe"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--i-understand-this-is-insecure"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_set_mode_rejects_bogus_mode() {
    let dir = scratch_config_dir("config-set-mode-bogus");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["config", "set-mode", "bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bogus"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn status_on_empty_config_exits_zero() {
    let dir = scratch_config_dir("status-empty");
    Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("credential_mode"))
        .stdout(predicate::str::contains("servers"))
        .stdout(predicate::str::contains("pinned_hosts"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn status_json_emits_structured_report() {
    let dir = scratch_config_dir("status-json");
    let assert = Command::cargo_bin("rustmote")
        .unwrap()
        .env("RUSTMOTE_CONFIG_DIR", &dir)
        .args(["status", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["credential_mode"], "prompt");
    assert_eq!(parsed["servers"], 0);
    assert!(parsed["viewer"]["status"].is_string());
    std::fs::remove_dir_all(&dir).ok();
}
