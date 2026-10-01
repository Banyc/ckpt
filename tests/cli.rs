//! The command line against the same tree.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
    ok(root, &["flag", "new", session.trim(), "--desc", "second"]);

    let value: serde_json::Value =
        serde_json::from_str(&ok(root, &["status", &flag, "--json"])).expect("json status");
    assert_eq!(value["matched"]["kind"], "flag");
    assert_eq!(value["matched"]["id"], flag.as_str());
    assert_eq!(value["flags"][0]["hits"], 1);
    assert_eq!(value["flags"][0]["counter"], 1);
    assert_eq!(
        value["flags"][1]["counter"], 2,
        "a report carries the counters in order"
    );

    let listed: serde_json::Value =
        serde_json::from_str(&ok(root, &["session", "list", "--json"])).expect("json list");
    assert_eq!(listed[0]["hits"], 1);
    assert_eq!(listed[0]["flags"], 2);
}

#[test]
fn every_command_accepts_both_written_id_forms() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let session = session.trim().to_owned();

    let sparse = format!("{}/{}", &session[..2], &session[2..]);
    let upper = format!(
        "{}/{}",
        session[..2].to_uppercase(),
        session[2..].to_uppercase()
    );

    let flag = ok(root, &["flag", "new", &sparse, "--desc", "f"]);
    assert_eq!(flag.trim().len(), 40, "a sparse session id is taken");
    assert!(ok(root, &["status", &upper]).contains(&session));
    assert!(ok(root, &["verify", &flag.trim().to_uppercase()]).contains("hits=1"));
}

#[test]
fn status_accepts_the_sparse_id_form() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let flag = ok(root, &["flag", "new", session.trim(), "--desc", "f"]);
    let flag = flag.trim().to_owned();

    let sparse = format!("{}/{}", &flag[..2], &flag[2..]);
    let status = ok(root, &["status", &sparse]);
    assert!(status.contains(&flag), "{status}");
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

    let session = ok(root, &["session", "new", "--desc", "s"]);
    let out = ckpt(root, &["verify", session.trim()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no flag"),
        "a session id is not a flag id"
    );
}

#[test]
fn the_default_root_is_ckpt_under_tmpdir() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .env("CKPT_ROOT", "")
        .env("TMPDIR", dir.path())
        .args(["session", "new", "--desc", "s"])
        .output()
        .expect("run ckpt");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let session = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let session = session.trim();
    assert!(
        dir.path()
            .join("ckpt")
            .join("sessions")
            .join(&session[..2])
            .join(&session[2..])
            .join("meta.json")
            .is_file(),
        "an empty CKPT_ROOT falls back to $TMPDIR/ckpt"
    );
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

#[test]
fn the_root_flag_wins_over_the_environment() {
    let from_env = tempfile::tempdir().expect("temp dir");
    let from_flag = tempfile::tempdir().expect("temp dir");
    let out = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .env("CKPT_ROOT", from_env.path())
        .arg("--root")
        .arg(from_flag.path())
        .args(["session", "new", "--desc", "s"])
        .output()
        .expect("run ckpt");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(from_flag.path().join("sessions").is_dir());
    assert!(!from_env.path().join("sessions").exists());
}

#[test]
fn the_note_is_stored_with_the_hit() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let session = session.trim().to_owned();
    let flag = ok(root, &["flag", "new", &session, "--desc", "f"]);
    let flag = flag.trim().to_owned();

    ok(root, &["verify", &flag, "--note", "saw it in the plot"]);

    let log = root
        .join("sessions")
        .join(&session[..2])
        .join(&session[2..])
        .join("flags")
        .join(&flag[..2])
        .join(&flag[2..])
        .join("hits.log");
    let text = fs::read_to_string(log).expect("read hits");
    assert!(text.contains("saw it in the plot"), "{text}");
}

#[test]
fn concurrent_verifications_from_separate_processes_all_land() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let session = session.trim().to_owned();
    let flag = ok(root, &["flag", "new", &session, "--desc", "f"]);
    let flag = flag.trim().to_owned();

    let verifications = 24;
    let mut children = Vec::new();
    for _ in 0..verifications {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_ckpt"))
                .arg("--root")
                .arg(root)
                .args(["verify", &flag])
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn ckpt"),
        );
    }
    let mut printed = Vec::new();
    for child in children {
        let out = child.wait_with_output().expect("wait");
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let count: u64 = text
            .trim()
            .rsplit_once("hits=")
            .expect("hits=")
            .1
            .parse()
            .expect("a count");
        printed.push(count);
    }
    assert_eq!(printed.len(), verifications);
    assert!(
        printed
            .iter()
            .all(|count| *count >= 1 && *count <= verifications as u64),
        "a printed count is a count the log held: {printed:?}"
    );

    let log = root
        .join("sessions")
        .join(&session[..2])
        .join(&session[2..])
        .join("flags")
        .join(&flag[..2])
        .join(&flag[2..])
        .join("hits.log");
    let text = fs::read_to_string(log).expect("read hits");
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), verifications, "one line per verification");
    assert!(
        lines
            .iter()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
        "no line was torn by a concurrent process"
    );
}

#[test]
fn concurrent_flag_additions_all_land() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let session = session.trim().to_owned();

    let additions = 12;
    let mut children = Vec::new();
    for _ in 0..additions {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_ckpt"))
                .arg("--root")
                .arg(root)
                .args(["flag", "new", &session, "--desc", "f"])
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn ckpt"),
        );
    }
    for mut child in children {
        assert!(child.wait().expect("wait").success());
    }

    let value: serde_json::Value =
        serde_json::from_str(&ok(root, &["status", &session, "--json"])).expect("json status");
    let flags = value["flags"].as_array().expect("flags");
    assert_eq!(flags.len(), additions, "every flag landed");

    let instants: Vec<jiff::Timestamp> = flags
        .iter()
        .map(|flag| {
            flag["created"]
                .as_str()
                .expect("created")
                .parse()
                .expect("instant")
        })
        .collect();
    assert!(
        instants.windows(2).all(|pair| pair[0] <= pair[1]),
        "the report is ordered by instant"
    );
    assert!(
        flags.iter().all(|flag| {
            flag["counter"]
                .as_u64()
                .is_some_and(|counter| counter >= 1 && counter <= additions as u64)
        }),
        "a counter is a count the session held"
    );
}

#[test]
#[cfg(unix)]
fn a_write_that_cannot_finish_leaves_no_record() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();

    // A file-size limit cuts the record short. A truncated record would never
    // read again, so the record must not be there at all.
    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -f 1; exec \"$CKPT\" --root \"$ROOT\" session new --desc \"$DESC\"")
        .env("CKPT", env!("CARGO_BIN_EXE_ckpt"))
        .env("ROOT", root)
        .env("DESC", "d".repeat(3_000))
        .output()
        .expect("run ckpt under a file-size limit");
    assert!(!out.status.success(), "the record could not be written");

    let listed = ok(root, &["session", "list"]);
    assert!(
        listed.starts_with("no sessions in "),
        "and the book still reads: {listed}"
    );
}

#[test]
#[cfg(unix)]
fn a_short_append_keeps_the_hits_before_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let session = session.trim().to_owned();
    let flag = ok(root, &["flag", "new", &session, "--desc", "f"]);
    let flag = flag.trim().to_owned();
    let log = root
        .join("sessions")
        .join(&session[..2])
        .join(&session[2..])
        .join("flags")
        .join(&flag[..2])
        .join(&flag[2..])
        .join("hits.log");

    assert!(ok(root, &["verify", &flag]).contains("hits=1"));
    let before = fs::read_to_string(&log).expect("read the log");
    assert_eq!(before.lines().count(), 1);

    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -f 1; exec \"$CKPT\" --root \"$ROOT\" verify \"$FLAG\" --note \"$NOTE\"")
        .env("CKPT", env!("CARGO_BIN_EXE_ckpt"))
        .env("ROOT", root)
        .env("FLAG", &flag)
        .env("NOTE", "x".repeat(2_000))
        .output()
        .expect("run ckpt under a file-size limit");
    assert_eq!(out.status.code(), Some(1), "nothing was appended");

    assert_eq!(
        fs::read_to_string(&log).expect("read the log"),
        before,
        "the hit already in the log survives the failed append"
    );
    assert!(ok(root, &["status", &flag]).contains("hits    1"));
}

#[test]
#[cfg(unix)]
fn a_short_append_leaves_the_log_as_it_was() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let session = session.trim().to_owned();
    let flag = ok(root, &["flag", "new", &session, "--desc", "f"]);
    let flag = flag.trim().to_owned();
    let log = root
        .join("sessions")
        .join(&session[..2])
        .join(&session[2..])
        .join("flags")
        .join(&flag[..2])
        .join(&flag[2..])
        .join("hits.log");

    // A file-size limit makes the append write only part of its line.
    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -f 1; exec \"$CKPT\" --root \"$ROOT\" verify \"$FLAG\" --note \"$NOTE\"")
        .env("CKPT", env!("CARGO_BIN_EXE_ckpt"))
        .env("ROOT", root)
        .env("FLAG", &flag)
        .env("NOTE", "x".repeat(2_000))
        .output()
        .expect("run ckpt under a file-size limit");

    assert_eq!(out.status.code(), Some(1), "nothing was appended");
    assert_eq!(
        fs::read_to_string(&log).expect("read the log"),
        "",
        "the log was left as it was, so the record still reads"
    );
    assert!(
        ok(root, &["status", &flag]).contains("hits    0"),
        "and the flag still reports its count"
    );
}

#[test]
#[cfg(unix)]
fn a_stdout_that_cannot_be_written_is_not_a_panic() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    let session = ok(root, &["session", "new", "--desc", "s"]);
    let flag = ok(root, &["flag", "new", session.trim(), "--desc", "f"]);
    let flag = flag.trim().to_owned();

    // A pipe whose reader is already gone: the writes cannot be delivered.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .arg("--root")
        .arg(root)
        .args(["verify", &flag])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ckpt");
    drop(child.stdout.take());
    let code = child.wait().expect("wait").code();
    assert_eq!(
        code,
        Some(3),
        "the hit was recorded and its count could not be delivered"
    );
    assert!(
        ok(root, &["status", &flag]).contains("hits    1"),
        "and it did land"
    );

    // A report that cannot be written is the same outcome for a command that
    // only read the book.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .arg("--root")
        .arg(root)
        .args(["status", &flag])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ckpt");
    drop(child.stdout.take());
    assert_eq!(child.wait().expect("wait").code(), Some(3));

    // A session whose id could not be delivered says the same.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .arg("--root")
        .arg(root)
        .args(["session", "new", "--desc", "t"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ckpt");
    drop(child.stdout.take());
    assert_eq!(child.wait().expect("wait").code(), Some(3));

    // A listing that could not be delivered.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .arg("--root")
        .arg(root)
        .args(["session", "list"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ckpt");
    drop(child.stdout.take());
    assert_eq!(child.wait().expect("wait").code(), Some(3));

    // And for one that created a record whose id could not be delivered.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ckpt"))
        .arg("--root")
        .arg(root)
        .args(["flag", "new", session.trim(), "--desc", "g"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ckpt");
    drop(child.stdout.take());
    let code = child.wait().expect("wait").code();
    assert_eq!(code, Some(3));
    assert!(
        ok(root, &["status", session.trim()]).contains("flags   2"),
        "and the flag it created is there"
    );
}
