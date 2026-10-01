//! The command line against the same tree.

use std::path::Path;
use std::process::{Command, Output};

fn ckpt(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("run ckpt")
}

fn ok(root: &Path, args: &[&str]) -> String {
    let out = ckpt(root, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

#[test]
fn the_command_line_round_trips_a_session_and_a_flag() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();

    let session = ok(root, &["session", "new", "--desc", "perf plots"]);
    let session = session.trim();
    assert_eq!(session.len(), 40, "session new prints the bare id");
    assert!(session.chars().all(|c| c.is_ascii_hexdigit()));

    let flag = ok(
        root,
        &["flag", "new", session, "--desc", "plot-read receipt"],
    );
    let flag = flag.trim().to_owned();
    assert_eq!(flag.len(), 40, "flag new prints the bare id");

    assert_eq!(
        ok(root, &["verify", &flag]).trim(),
        format!("flag {flag} hits=1")
    );

    // The sparse form is accepted wherever an id is.
    let sparse = format!("{}/{}", &flag[..2], &flag[2..]);
    assert_eq!(
        ok(root, &["verify", &sparse]).trim(),
        format!("flag {flag} hits=2")
    );

    let status = ok(root, &["status", &flag]);
    assert!(status.contains("plot-read receipt"), "{status}");
    assert!(status.contains("hits    2"), "{status}");

    let listed = ok(root, &["session", "list"]);
    assert!(listed.contains("flags 1  hits 2"), "{listed}");
}

#[test]
fn json_reports_carry_the_id_they_matched() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let flag = ok(root, &["flag", "new", session.trim(), "--desc", "f"]);
    let flag = flag.trim().to_owned();
    ok(root, &["verify", &flag]);

    let value: serde_json::Value =
        serde_json::from_str(&ok(root, &["status", &flag, "--json"])).expect("json status");
    assert_eq!(value["matched"]["kind"], "flag");
    assert_eq!(value["matched"]["id"], flag.as_str());
    assert_eq!(value["flags"][0]["hits"], 1);

    let listed: serde_json::Value =
        serde_json::from_str(&ok(root, &["session", "list", "--json"])).expect("json list");
    assert_eq!(listed[0]["hits"], 1);
    assert_eq!(listed[0]["flags"], 1);
}

#[test]
fn a_bad_id_exits_one_with_a_readable_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();

    let out = ckpt(root, &["status", "deadbeef"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("ckpt: "), "{err}");
    assert!(err.contains("deadbeef"), "{err}");

    let out = ckpt(root, &["verify", &"0".repeat(40)]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no flag"),
        "an unknown flag is named"
    );

    let out = ckpt(root, &["flag", "new", &"1".repeat(40), "--desc", "f"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no session"));
}

fn ckpt_env(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .env("CKPT_ROOT", root)
        .args(args)
        .output()
        .expect("run ckpt")
}

#[test]
fn ckpt_root_selects_the_store_when_no_root_flag_is_given() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = ckpt_env(
        dir.path(),
        &["session", "new", "--desc", "from the environment"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let session = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let session = session.trim();
    assert_eq!(session.len(), 40);
    assert!(
        dir.path()
            .join("sessions")
            .join(&session[..2])
            .join(&session[2..])
            .join("meta.json")
            .is_file(),
        "the session landed under CKPT_ROOT"
    );
}

#[test]
fn an_empty_store_reports_itself() {
    let dir = tempfile::tempdir().expect("temp dir");
    let listed = ok(dir.path(), &["session", "list"]);
    assert!(listed.starts_with("no sessions in "), "{listed}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&ok(dir.path(), &["session", "list", "--json"]))
            .expect("json")
            .as_array()
            .expect("array")
            .is_empty()
    );
}
