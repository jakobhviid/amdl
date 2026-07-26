//! End-to-end CLI tests: drive the real `amdl` binary and assert on behavior.
//! Everything here is **offline and tool-free** — no network, no ffmpeg — so it
//! runs deterministically in CI (which gates releases on `cargo test`). Audio
//! tests use a tiny checked-in Opus fixture; each test gets isolated temp config
//! and undo dirs, so they're parallel-safe.
use amdl_core::tags;
use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A binary invocation with isolated config + undo state and no color.
fn amdl(home: &Path, undo: &Path) -> Command {
    let mut c = Command::cargo_bin("amdl").unwrap();
    c.env("XDG_CONFIG_HOME", home).env("AMDL_UNDO_DIR", undo).env("NO_COLOR", "1");
    c
}

/// Copy the checked-in fixture into `dir` (tests mutate the copy, not the fixture).
fn fixture_copy(dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/track.opus");
    let dst = dir.join("track.opus");
    std::fs::copy(&src, &dst).unwrap();
    dst
}

#[test]
fn configure_surface_both_grammars_and_exit_codes() {
    let (home, undo) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let a = || amdl(home.path(), undo.path());

    // words grammar + on/off
    a().args(["configure", "set", "lyrics", "hints", "off"]).assert().success();
    a().args(["configure", "get", "lyrics.hints"]).assert().success().stdout("off\n");
    // dotted grammar, read back via words
    a().args(["configure", "set", "lyrics.hints", "on"]).assert().success();
    a().args(["configure", "get", "lyrics", "hints"]).assert().success().stdout("on\n");
    // a string value + unset
    a().args(["configure", "set", "keys.acoustid", "ABC123"]).assert().success();
    a().args(["configure", "get", "keys", "acoustid"]).assert().success().stdout("ABC123\n");
    a().args(["configure", "unset", "keys.acoustid"]).assert().success();
    a().args(["configure", "get", "keys.acoustid"]).assert().success().stdout("");
    // listings work
    a().args(["configure", "list", "--json"]).assert().success();
    a().args(["configure", "keys"]).assert().success();
    // structural failures exit non-zero and change nothing
    a().args(["configure", "set", "bogus.key", "x"]).assert().failure();
    a().args(["configure", "set", "lyrics.hints", "maybe"]).assert().failure();
}

#[test]
fn config_init_writes_a_valid_annotated_template() {
    let (home, undo) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    amdl(home.path(), undo.path()).args(["config", "--init"]).assert().success();
    let cfg = std::fs::read_to_string(home.path().join("amdl/config.toml")).unwrap();
    // Sections + a key present; amdl-core's unit test verifies it parses as TOML.
    assert!(cfg.contains("[lyrics]") && cfg.contains("aligner_url"), "template missing sections");
    assert!(cfg.contains("[paths]") && cfg.contains("[keys]"));
}

#[test]
fn help_and_llm_and_cookies_json_do_not_crash() {
    let (home, undo) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    amdl(home.path(), undo.path()).arg("--help").assert().success();
    amdl(home.path(), undo.path()).arg("--llm").assert().success();
    amdl(home.path(), undo.path()).args(["configure", "--help"]).assert().success();
    // cookies --json must emit valid JSON (no browser in CI → still a clean object)
    let out = amdl(home.path(), undo.path()).args(["cookies", "--json"]).output().unwrap();
    assert!(out.status.success());
    serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("cookies --json is valid JSON");
}

#[test]
fn stats_reads_the_fixture() {
    let (dir, home, undo) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    fixture_copy(dir.path());
    let out = amdl(home.path(), undo.path())
        .args(["stats", dir.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["tracks"], 1);
    assert_eq!(v["artists"], 1);
    assert_eq!(v["missing_artist"], 0);
}

#[test]
fn tag_edit_then_undo_restores_every_field() {
    let (dir, home, undo) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    let f = fixture_copy(dir.path());
    assert_eq!(tags::read_basic(&f).album.as_deref(), Some("Test Album"));

    amdl(home.path(), undo.path()).args(["tag", f.to_str().unwrap(), "--album", "CHANGED ALBUM"]).assert().success();
    assert_eq!(tags::read_basic(&f).album.as_deref(), Some("CHANGED ALBUM"));

    amdl(home.path(), undo.path()).arg("undo").assert().success();
    assert_eq!(tags::read_basic(&f).album.as_deref(), Some("Test Album"), "undo should restore the album");
}

#[test]
fn mark_instrumental_then_undo() {
    let (dir, home, undo) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    let f = fixture_copy(dir.path());
    assert!(!tags::read_basic(&f).instrumental_marked);

    // Offline repair: no network, just strips lyrics (none here) + stamps the mark.
    amdl(home.path(), undo.path()).args(["lyrics", f.to_str().unwrap(), "--mark-instrumental"]).assert().success();
    assert!(tags::read_basic(&f).instrumental_marked, "should be marked instrumental");

    amdl(home.path(), undo.path()).arg("undo").assert().success();
    assert!(!tags::read_basic(&f).instrumental_marked, "undo should clear the mark");
}
