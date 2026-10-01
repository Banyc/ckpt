//! The record book's behavior over its on-disk tree.

use std::fs;
use std::path::{Path, PathBuf};

use ckpt::{Error, FlagId, Id, SessionId, Store, Target, Verify};
use jiff::Timestamp;
use tempfile::TempDir;

fn book() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::at(dir.path());
    (dir, store)
}

fn at(second: i64) -> Option<Timestamp> {
    Some(Timestamp::from_second(second).expect("valid timestamp"))
}

fn verify_at(store: &Store, flag: &FlagId, second: i64) -> u64 {
    store
        .verify(
            flag,
            &Verify {
                note: None,
                at: at(second),
            },
        )
        .expect("verify")
        .hits
}

fn session_dir(root: &Path, session: &SessionId) -> PathBuf {
    let (shard, rest) = session.as_id().sparse();
    root.join("sessions").join(shard).join(rest)
}

fn flag_dir(root: &Path, session: &SessionId, flag: &FlagId) -> PathBuf {
    let (shard, rest) = flag.as_id().sparse();
    session_dir(root, session)
        .join("flags")
        .join(shard)
        .join(rest)
}

#[test]
fn a_session_lands_in_the_sparse_tree() {
    let (dir, store) = book();
    let session = store.session_new("perf plots").expect("session");

    let (shard, rest) = session.as_id().sparse();
    assert_eq!(shard.len(), 2);
    assert_eq!(rest.len(), 38);

    let session_dir = session_dir(dir.path(), &session);
    assert!(session_dir.join("meta.json").is_file(), "meta.json exists");
    assert!(session_dir.join("flags").is_dir(), "flags/ exists");
    assert!(
        session_dir
            .join("flags")
            .read_dir()
            .expect("read flags")
            .next()
            .is_none(),
        "a fresh session has no flags"
    );
    assert!(
        fs::read_to_string(session_dir.join("meta.json"))
            .expect("read meta")
            .contains("perf plots"),
        "the description is stored"
    );
}

#[test]
fn a_flag_lands_in_the_sparse_tree_with_an_empty_log() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "plot-read receipt").expect("flag");

    let flag_dir = flag_dir(dir.path(), &session, &flag);
    assert!(flag_dir.join("meta.json").is_file(), "meta.json exists");
    assert_eq!(
        fs::read_to_string(flag_dir.join("hits.log")).expect("read hits"),
        "",
        "a fresh flag has an empty hit log"
    );
}

#[test]
fn verify_counts_each_hit() {
    let (_dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    assert_eq!(verify_at(&store, &flag, 1_000), 1);
    assert_eq!(verify_at(&store, &flag, 1_001), 2);
    assert_eq!(verify_at(&store, &flag, 1_002), 3);

    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(status.flags.len(), 1);
    assert_eq!(status.flags[0].hits, 3);
    assert_eq!(status.flags[0].last_hit, at(1_002));
}

#[test]
fn hits_are_counted_per_flag() {
    let (_dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");

    verify_at(&store, &first, 1_000);
    verify_at(&store, &first, 1_001);
    verify_at(&store, &second, 1_002);

    let status = store.status(session.as_id()).expect("status");
    let hits = |flag: &FlagId| {
        status
            .flags
            .iter()
            .find(|entry| &entry.id == flag)
            .expect("flag is listed")
            .hits
    };
    assert_eq!(hits(&first), 2);
    assert_eq!(hits(&second), 1);
}

#[test]
fn a_flag_id_names_the_session_that_holds_it() {
    let (_dir, store) = book();
    let first = store.session_new("first session").expect("session");
    let second = store.session_new("second session").expect("session");
    let flag = store.flag_new(&second, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let by_flag = store.status(flag.as_id()).expect("status");
    assert_eq!(by_flag.session, second);
    assert_eq!(
        by_flag.matched,
        Target::Flag { id: flag.clone() },
        "the report says which id was asked for"
    );
    assert_eq!(by_flag.desc, "second session");
    assert_eq!(by_flag.flags.len(), 1);

    let by_session = store.status(first.as_id()).expect("status");
    assert_eq!(by_session.session, first);
    assert_eq!(by_session.matched, Target::Session);
    assert!(by_session.flags.is_empty());
}

#[test]
fn status_reads_the_tree_on_every_call() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    assert_eq!(store.status(flag.as_id()).expect("status").flags[0].hits, 0);

    // A second writer appends a line without going through this Store.
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    let mut text = fs::read_to_string(&log).expect("read hits");
    text.push_str("{\"ts\":\"2030-01-01T00:00:00Z\"}\n");
    fs::write(&log, text).expect("append hit");

    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(status.flags[0].hits, 1, "no cached count survives a write");
    assert_eq!(
        status.flags[0].last_hit,
        at(1_893_456_000),
        "the appended instant is read back"
    );
}

#[test]
fn hits_are_written_as_rfc3339() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    store
        .verify(
            &flag,
            &Verify {
                note: Some("saw it in the plot".to_owned()),
                at: at(1_000_000_000),
            },
        )
        .expect("verify");

    let log = fs::read_to_string(flag_dir(dir.path(), &session, &flag).join("hits.log"))
        .expect("read hits");
    assert!(log.contains("\"ts\":\"2001-09-09T01:46:40Z\""), "{log}");
    assert!(log.contains("saw it in the plot"), "{log}");
}

#[test]
fn unknown_ids_are_not_found() {
    let (_dir, store) = book();
    assert!(store.sessions().expect("sessions").is_empty());

    let absent_session = SessionId::generate();
    assert!(matches!(
        store.status(absent_session.as_id()),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        store.flag_new(&absent_session, "flag"),
        Err(Error::NotFound { .. })
    ));

    let absent_flag = FlagId::generate();
    assert!(matches!(
        store.verify(&absent_flag, &Verify::new()),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn malformed_ids_are_rejected() {
    let inputs = [
        String::new(),
        "abc".to_owned(),
        "z".repeat(40),
        "0".repeat(39),
        "0".repeat(41),
        "01/2345".to_owned(),
        "012/3456789abcdef0123456789abcdef0123456".to_owned(),
        format!("0/{}", "1".repeat(39)),
        format!("{}/{}", "0".repeat(3), "1".repeat(37)),
    ];
    for input in &inputs {
        assert!(
            matches!(Id::parse(input), Err(Error::InvalidId { .. })),
            "`{input}` must be rejected"
        );
    }
}

#[test]
fn ids_accept_flat_sparse_and_uppercase_forms() {
    let flat = "0123456789abcdef0123456789abcdef01234567";
    let sparse = format!("{}/{}", &flat[..2], &flat[2..]);

    assert_eq!(Id::parse(flat).expect("flat").as_str(), flat);
    assert_eq!(Id::parse(&sparse).expect("sparse").as_str(), flat);
    assert_eq!(
        Id::parse(&flat.to_uppercase()).expect("uppercase").as_str(),
        flat,
        "object names are stored lowercased"
    );
    assert_eq!(
        Id::parse(&format!(
            "{}/{}",
            flat[..2].to_uppercase(),
            flat[2..].to_uppercase()
        ))
        .expect("uppercase sparse")
        .as_str(),
        flat,
        "a sparse argument is normalized like a flat one"
    );
}

#[test]
fn dotfiles_in_owned_directories_are_ignored() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let (shard, _) = session.as_id().sparse();
    fs::write(
        dir.path().join("sessions").join(shard).join(".DS_Store"),
        "junk",
    )
    .expect("write dotfile");
    fs::write(
        flag_dir(dir.path(), &session, &flag).join(".DS_Store"),
        "junk",
    )
    .expect("write dotfile");

    assert_eq!(store.status(flag.as_id()).expect("status").flags[0].hits, 1);
    assert_eq!(store.sessions().expect("sessions").len(), 1);
}

#[test]
fn a_foreign_entry_in_an_owned_directory_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let (shard, _) = session.as_id().sparse();
    fs::create_dir(dir.path().join("sessions").join(shard).join("not-an-id")).expect("create");

    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
fn a_damaged_record_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    let mut text = fs::read_to_string(&log).expect("read hits");
    text.push_str("not json\n");
    fs::write(&log, text).expect("damage log");
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "an unparseable hit line is reported, not skipped"
    );

    let meta = session_dir(dir.path(), &session).join("meta.json");
    fs::write(&meta, "not json").expect("damage meta");
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn sessions_are_listed_with_their_totals() {
    let (_dir, store) = book();
    let first = store.session_new("first").expect("session");
    let second = store.session_new("second").expect("session");
    let first_flag = store.flag_new(&first, "a").expect("flag");
    store.flag_new(&first, "b").expect("flag");
    verify_at(&store, &first_flag, 1_000);

    let sessions = store.sessions().expect("sessions");
    assert_eq!(sessions.len(), 2);
    let listed = |session: &SessionId| {
        sessions
            .iter()
            .find(|summary| &summary.session == session)
            .expect("session is listed")
    };
    assert_eq!(listed(&first).flags, 2);
    assert_eq!(listed(&first).hits, 1);
    assert_eq!(listed(&second).flags, 0);
    assert_eq!(listed(&second).hits, 0);
    assert!(
        sessions[0].session <= sessions[1].session,
        "sessions are ordered by id"
    );
}

#[test]
fn concurrent_verifications_all_land_intact() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    let threads = 16;
    let per_thread = 50;
    let counts = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                for _ in 0..per_thread {
                    let status = store.verify(&flag, &Verify::new()).expect("verify");
                    counts.lock().expect("lock").push(status.hits);
                }
            });
        }
    });

    let expected = (threads * per_thread) as u64;
    let counts = counts.into_inner().expect("lock");
    assert_eq!(counts.len() as u64, expected);
    assert!(
        counts.iter().all(|count| *count >= 1 && *count <= expected),
        "a reported count is a count the log held"
    );
    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(status.flags[0].hits, expected);

    let log = fs::read_to_string(flag_dir(dir.path(), &session, &flag).join("hits.log"))
        .expect("read hits");
    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(lines.len() as u64, expected, "one line per verification");
    assert!(
        lines
            .iter()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
        "no line was torn by a concurrent append"
    );
}

#[test]
fn a_non_canonical_path_split_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flat = session.as_id().as_str().to_owned();

    // The same object name spelled with a 1/39 split instead of 2/38.
    let dir_1_39 = dir
        .path()
        .join("sessions")
        .join(&flat[..1])
        .join(&flat[1..]);
    fs::create_dir_all(&dir_1_39).expect("create");
    fs::copy(
        session_dir(dir.path(), &session).join("meta.json"),
        dir_1_39.join("meta.json"),
    )
    .expect("copy meta");

    assert!(
        matches!(store.sessions(), Err(Error::Corrupt { .. })),
        "a second spelling of the same id is a foreign entry, not a second session"
    );
}

#[test]
fn a_non_canonical_directory_case_is_reported() {
    let (dir, store) = book();
    let rest = dir.path().join("sessions").join("F2").join("0".repeat(38));
    fs::create_dir_all(&rest).expect("create");
    fs::write(rest.join("meta.json"), "{}").expect("write meta");

    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
fn a_missing_flag_directory_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    fs::remove_dir_all(session_dir(dir.path(), &session).join("flags")).expect("remove flags");

    assert!(
        matches!(store.status(session.as_id()), Err(Error::Io { .. })),
        "a session whose flags/ is gone is a broken record, not an empty one"
    );
    assert!(matches!(store.sessions(), Err(Error::Io { .. })));
}

#[test]
fn a_missing_hit_log_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    fs::remove_file(flag_dir(dir.path(), &session, &flag).join("hits.log")).expect("remove log");

    assert!(matches!(store.status(flag.as_id()), Err(Error::Io { .. })));
}

#[test]
fn a_failed_verification_writes_nothing() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    assert!(store.verify(&FlagId::generate(), &Verify::new()).is_err());

    let log = fs::read_to_string(flag_dir(dir.path(), &session, &flag).join("hits.log"))
        .expect("read hits");
    assert_eq!(log, "", "a rejected verification leaves the log untouched");
}

#[test]
fn descriptions_round_trip() {
    for desc in ["", "unicode: σ 性能", &"x".repeat(4096)] {
        let (_dir, store) = book();
        let session = store.session_new(desc).expect("session");
        store.flag_new(&session, desc).expect("flag");

        let status = store.status(session.as_id()).expect("status");
        assert_eq!(status.desc, desc);
        assert_eq!(status.flags[0].desc, desc);
    }
}

#[test]
fn a_file_where_a_record_belongs_is_reported() {
    let (dir, store) = book();
    let stray = dir.path().join("sessions").join("ab").join("0".repeat(38));
    fs::create_dir_all(stray.parent().expect("parent")).expect("create");
    fs::write(&stray, "not a record").expect("write stray");

    assert!(
        matches!(store.sessions(), Err(Error::Corrupt { .. })),
        "a file at an object name is a foreign entry, not an absent session"
    );
}

#[test]
fn a_file_where_a_flag_belongs_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let stray = session_dir(dir.path(), &session)
        .join("flags")
        .join("ab")
        .join("0".repeat(38));
    fs::create_dir_all(stray.parent().expect("parent")).expect("create");
    fs::write(&stray, "not a record").expect("write stray");

    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_blank_hit_line_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");

    assert_eq!(store.status(flag.as_id()).expect("status").flags[0].hits, 0);

    fs::write(&log, "\n").expect("write blank line");
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "the writer only appends complete records, so a blank line is damage"
    );
}

#[test]
fn a_flag_under_a_session_without_meta_is_not_found() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_file(session_dir(dir.path(), &session).join("meta.json")).expect("remove meta");

    assert!(
        matches!(store.status(flag.as_id()), Err(Error::NotFound { .. })),
        "a session exists once its meta.json does"
    );
    assert!(matches!(
        store.flag_new(&session, "another"),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn a_flag_lookup_counts_only_its_own_session() {
    let (_dir, store) = book();
    let first = store.session_new("first").expect("session");
    let second = store.session_new("second").expect("session");
    let first_flag = store.flag_new(&first, "a").expect("flag");
    let second_flag = store.flag_new(&second, "b").expect("flag");
    verify_at(&store, &first_flag, 1_000);
    verify_at(&store, &first_flag, 1_001);
    verify_at(&store, &second_flag, 1_002);

    let status = store.status(second_flag.as_id()).expect("status");
    assert_eq!(status.session, second);
    assert_eq!(status.flags.len(), 1);
    assert_eq!(status.flags[0].id, second_flag);
    assert_eq!(status.flags[0].hits, 1);
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create");
    for entry in fs::read_dir(from).expect("read") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

#[test]
#[cfg(unix)]
fn a_symlink_at_a_store_directory_is_reported() {
    let (dir, store) = book();
    let outside = TempDir::new().expect("temp dir");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("sessions")).expect("symlink");

    assert!(
        matches!(store.sessions(), Err(Error::Corrupt { .. })),
        "a symlink where the store keeps sessions is a foreign entry"
    );
}

#[test]
#[cfg(unix)]
fn a_dangling_symlink_at_a_store_directory_is_reported() {
    let (dir, store) = book();
    std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("sessions"))
        .expect("symlink");

    assert!(
        matches!(store.sessions(), Err(Error::Corrupt { .. })),
        "a link to nothing is not an empty store"
    );
}

#[test]
#[cfg(unix)]
fn a_symlink_at_a_flag_object_name_is_not_written_through() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let outside = TempDir::new().expect("temp dir");
    let moved = outside.path().join("moved");
    let link = flag_dir(dir.path(), &session, &flag);
    fs::rename(&link, &moved).expect("move the record out of the tree");
    std::os::unix::fs::symlink(&moved, &link).expect("symlink");

    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(moved.join("hits.log")).expect("read log"),
        "",
        "a hit is never written through a link"
    );
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_file_at_a_flag_object_name_is_reported_by_the_flag_path() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let stray = session_dir(dir.path(), &session)
        .join("flags")
        .join("ab")
        .join("0".repeat(38));
    fs::create_dir_all(stray.parent().expect("parent")).expect("create");
    fs::write(&stray, "not a record").expect("write stray");

    let id = FlagId::parse(&format!("ab{}", "0".repeat(38))).expect("id");
    assert!(
        matches!(store.status(id.as_id()), Err(Error::Corrupt { .. })),
        "the flag path reports what the session path reports"
    );
    assert!(matches!(
        store.verify(&id, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_meta_that_is_not_a_regular_file_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let meta = session_dir(dir.path(), &session).join("meta.json");
    fs::remove_file(&meta).expect("remove meta");
    fs::create_dir(&meta).expect("put a directory at meta.json");

    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_write_does_not_heal_a_missing_flag_directory() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flags = session_dir(dir.path(), &session).join("flags");
    fs::remove_dir_all(&flags).expect("remove flags");

    assert!(matches!(
        store.flag_new(&session, "flag"),
        Err(Error::Io { .. })
    ));
    assert!(!flags.exists(), "nothing was recreated");
}

#[test]
fn a_damaged_log_is_reported_without_appending() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::write(&log, "damaged\n").expect("damage the log");

    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(&log).expect("read log"),
        "damaged\n",
        "a rejected verification leaves the log untouched"
    );
}

#[test]
fn a_flag_recorded_under_two_sessions_is_reported() {
    let (dir, store) = book();
    let first = store.session_new("first").expect("session");
    let second = store.session_new("second").expect("session");
    let flag = store.flag_new(&first, "flag").expect("flag");

    let from = flag_dir(dir.path(), &first, &flag);
    let to = flag_dir(dir.path(), &second, &flag);
    fs::create_dir_all(to.parent().expect("parent")).expect("create");
    copy_tree(&from, &to);

    assert!(
        matches!(
            store.verify(&flag, &Verify::new()),
            Err(Error::Corrupt { .. })
        ),
        "a hit must not land in whichever session the walk reached first"
    );
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_foreign_entry_in_a_flag_shard_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    store.flag_new(&session, "flag").expect("flag");
    let shard = session_dir(dir.path(), &session).join("flags").join("ab");
    fs::create_dir_all(&shard).expect("create");
    fs::create_dir(shard.join("not-an-id")).expect("create foreign entry");

    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn the_last_hit_is_the_last_appended() {
    let (_dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    assert_eq!(
        store.status(flag.as_id()).expect("status").flags[0].last_hit,
        None,
        "a flag with no hits has no last hit"
    );

    verify_at(&store, &flag, 2_000);
    verify_at(&store, &flag, 1_000);

    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(status.flags[0].hits, 2);
    assert_eq!(
        status.flags[0].last_hit,
        at(1_000),
        "the log's order decides, not the largest instant"
    );
}

#[test]
// macOS rejects a non-UTF-8 file name at the syscall, so an entry this branch
// guards against cannot be created there. Linux filesystems store such names.
#[cfg(target_os = "linux")]
fn a_non_utf8_entry_name_is_reported() {
    use std::os::unix::ffi::OsStrExt;

    let (dir, store) = book();
    let shard = dir.path().join("sessions").join("ab");
    fs::create_dir_all(&shard).expect("create");
    fs::create_dir(shard.join(std::ffi::OsStr::from_bytes(b"\xff\xfe"))).expect("create odd name");

    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
fn a_final_line_without_a_newline_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::write(&log, "{\"ts\":\"2030-01-01T00:00:00Z\"}").expect("write without a newline");

    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "the writer terminates every line, so an unterminated one is damage"
    );
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(&log).expect("read log"),
        "{\"ts\":\"2030-01-01T00:00:00Z\"}",
        "a rejected verification appends nothing"
    );
}

#[test]
fn a_flag_lookup_reports_a_missing_flag_directory() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_dir_all(session_dir(dir.path(), &session).join("flags")).expect("remove flags");

    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Io { .. })),
        "a flag lookup reports the damaged session, it does not answer 'no such flag'"
    );
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Io { .. })
    ));
}

#[test]
fn an_interrupted_write_is_invisible() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flat = session.as_id().as_str().to_owned();
    fs::remove_file(session_dir(dir.path(), &session).join("meta.json")).expect("remove meta");

    assert!(
        store.sessions().expect("sessions").is_empty(),
        "a directory with no meta.json is an interrupted write, not a record"
    );

    // The same at the flag level: hits.log is created before meta.json, so an
    // interrupted flag_new leaves exactly this.
    let session = store.session_new("other").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_file(flag_dir(dir.path(), &session, &flag).join("meta.json")).expect("remove meta");
    let status = store.status(session.as_id()).expect("status");
    assert!(
        status.flags.is_empty(),
        "the remains are not listed as a flag"
    );

    assert!(
        !flat.is_empty() && store.status(session.as_id()).is_ok(),
        "the store keeps answering for the sessions that are complete"
    );
}

#[test]
fn a_record_of_the_wrong_shape_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    // Valid JSON, but not a hit.
    fs::write(
        flag_dir(dir.path(), &session, &flag).join("hits.log"),
        "{\"note\":\"x\"}\n",
    )
    .expect("write a wrong-shaped hit");
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));

    // Valid JSON, but not a record.
    fs::write(session_dir(dir.path(), &session).join("meta.json"), "{}\n")
        .expect("write a wrong-shaped meta");
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn sessions_as_a_regular_file_is_reported() {
    let (dir, store) = book();
    fs::create_dir_all(dir.path()).expect("create root");
    fs::write(dir.path().join("sessions"), "not a directory").expect("write sessions");

    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
fn a_non_canonical_rest_is_reported() {
    // A canonical shard with a 37-character rest.
    let (dir, store) = book();
    let shard = dir.path().join("sessions").join("ab");
    fs::create_dir_all(shard.join("0".repeat(37))).expect("create short rest");
    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));

    // A canonical shard with an uppercase rest: the case a case-folding read
    // would turn into a second spelling of an object name already stored.
    let (dir, store) = book();
    let shard = dir.path().join("sessions").join("ab");
    fs::create_dir_all(shard.join("AB".repeat(19))).expect("create upper rest");
    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));

    // The same under a session's flags/ entry.
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let shard = session_dir(dir.path(), &session).join("flags").join("ab");
    fs::create_dir_all(shard.join("0".repeat(37))).expect("create short rest");
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_symlinked_flag_shard_on_the_read_path_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let outside = TempDir::new().expect("temp dir");
    let shard = flag_dir(dir.path(), &session, &flag)
        .parent()
        .expect("parent")
        .to_path_buf();
    let moved = outside.path().join("shard");
    fs::rename(&shard, &moved).expect("move the shard out");
    std::os::unix::fs::symlink(&moved, &shard).expect("symlink");

    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    let log = moved.join(flag.as_id().sparse().1).join("hits.log");
    assert_eq!(
        fs::read_to_string(log).expect("read log").lines().count(),
        1,
        "the rejected verification appended nothing"
    );
}

#[test]
fn a_hit_log_that_is_a_directory_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::remove_file(&log).expect("remove log");
    fs::create_dir(&log).expect("put a directory at hits.log");

    assert!(
        matches!(
            store.verify(&flag, &Verify::new()),
            Err(Error::Corrupt { .. })
        ),
        "a known flag whose log cannot be written is an error, not a silent hit"
    );
    assert!(log.is_dir(), "nothing was written over it");
}

#[test]
fn a_foreign_name_inside_a_record_directory_is_ignored() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    // A record directory is read by name, so names the store does not own there
    // are left alone rather than reported.
    fs::write(session_dir(dir.path(), &session).join("bogus"), "x").expect("write");
    fs::write(flag_dir(dir.path(), &session, &flag).join("bogus"), "x").expect("write");

    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(status.flags[0].hits, 1);
    assert_eq!(store.sessions().expect("sessions").len(), 1);
}

#[test]
fn an_empty_foreign_shard_is_reported() {
    let (dir, store) = book();
    fs::create_dir_all(dir.path().join("sessions").join("zz")).expect("create");

    assert!(
        matches!(store.sessions(), Err(Error::Corrupt { .. })),
        "a foreign shard is reported even when it holds nothing"
    );
}

#[test]
fn an_empty_foreign_flag_shard_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    fs::create_dir_all(session_dir(dir.path(), &session).join("flags").join("zz")).expect("create");

    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_flag_is_not_added_to_a_corrupt_session() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    fs::write(
        session_dir(dir.path(), &session).join("meta.json"),
        "not json",
    )
    .expect("damage meta");

    assert!(matches!(
        store.flag_new(&session, "flag"),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_symlinked_shard_is_not_written_through() {
    let (dir, store) = book();
    let outside = TempDir::new().expect("temp dir");
    let sessions = dir.path().join("sessions");
    fs::create_dir_all(&sessions).expect("create");
    for high in "0123456789abcdef".chars() {
        for low in "0123456789abcdef".chars() {
            std::os::unix::fs::symlink(outside.path(), sessions.join(format!("{high}{low}")))
                .expect("symlink a shard");
        }
    }

    assert!(
        matches!(store.session_new("session"), Err(Error::Corrupt { .. })),
        "a link where a shard belongs is reported, not followed"
    );
    assert!(
        outside.path().read_dir().expect("read").next().is_none(),
        "nothing was created outside the store"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_flag_shard_is_not_written_through() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let outside = TempDir::new().expect("temp dir");
    let flags = session_dir(dir.path(), &session).join("flags");
    for high in "0123456789abcdef".chars() {
        for low in "0123456789abcdef".chars() {
            std::os::unix::fs::symlink(outside.path(), flags.join(format!("{high}{low}")))
                .expect("symlink a shard");
        }
    }

    assert!(
        matches!(store.flag_new(&session, "flag"), Err(Error::Corrupt { .. })),
        "a link where a flag shard belongs is reported, not followed"
    );
    assert!(
        outside.path().read_dir().expect("read").next().is_none(),
        "nothing was created outside the store"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_hit_log_is_not_written_through() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let outside = TempDir::new().expect("temp dir");
    let target = outside.path().join("elsewhere.log");
    // A parseable line, so only the link check stops the append.
    fs::write(&target, "{\"ts\":\"2030-01-01T00:00:00Z\"}\n").expect("write target");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::remove_file(&log).expect("remove log");
    std::os::unix::fs::symlink(&target, &log).expect("symlink");

    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "{\"ts\":\"2030-01-01T00:00:00Z\"}\n",
        "the file outside the store is untouched"
    );
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_symlinked_meta_is_not_read_through() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let outside = TempDir::new().expect("temp dir");
    let target = outside.path().join("elsewhere.json");
    fs::write(
        &target,
        "{\"desc\":\"borrowed\",\"created\":\"2030-01-01T00:00:00Z\"}\n",
    )
    .expect("write target");
    let meta = session_dir(dir.path(), &session).join("meta.json");
    fs::remove_file(&meta).expect("remove meta");
    std::os::unix::fs::symlink(&target, &meta).expect("symlink");

    assert!(
        matches!(store.status(session.as_id()), Err(Error::Corrupt { .. })),
        "a borrowed record is refused"
    );
}

fn meta_of(root: &Path, session: &SessionId, flag: &FlagId) -> PathBuf {
    flag_dir(root, session, flag).join("meta.json")
}

fn set_created(path: &Path, second: i64) {
    let text = fs::read_to_string(path).expect("read meta");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("meta json");
    value["created"] = serde_json::Value::String(
        Timestamp::from_second(second)
            .expect("valid instant")
            .to_string(),
    );
    fs::write(path, format!("{value}\n")).expect("write meta");
}

#[test]
fn flags_are_ordered_by_instant_then_counter() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");
    let third = store.flag_new(&session, "third").expect("flag");

    let status = store.status(session.as_id()).expect("status");
    let order: Vec<&FlagId> = status.flags.iter().map(|flag| &flag.id).collect();
    assert_eq!(order, vec![&first, &second, &third], "creation order");
    let counters: Vec<u64> = status.flags.iter().map(|flag| flag.counter).collect();
    assert_eq!(counters, vec![1, 2, 3]);

    // Hand-set instants: the later instant comes last whatever the counter says,
    // and flags sharing an instant are ordered by the counter.
    set_created(&meta_of(dir.path(), &session, &first), 2_000);
    set_created(&meta_of(dir.path(), &session, &second), 1_000);
    set_created(&meta_of(dir.path(), &session, &third), 1_000);

    let status = store.status(session.as_id()).expect("status");
    let order: Vec<&FlagId> = status.flags.iter().map(|flag| &flag.id).collect();
    assert_eq!(
        order,
        vec![&second, &third, &first],
        "instant first, counter where instants are equal"
    );
}

#[test]
fn a_flag_meta_without_a_counter_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::write(
        meta_of(dir.path(), &session, &flag),
        "{\"desc\":\"flag\",\"created\":\"2030-01-01T00:00:00Z\"}\n",
    )
    .expect("write meta without a counter");

    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_symlinked_flags_directory_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let outside = TempDir::new().expect("temp dir");
    let flags = session_dir(dir.path(), &session).join("flags");
    let moved = outside.path().join("flags");
    fs::rename(&flags, &moved).expect("move flags out of the tree");
    std::os::unix::fs::symlink(&moved, &flags).expect("symlink");

    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(moved.join(flag.sparse_path()).join("hits.log"))
            .expect("read log")
            .lines()
            .count(),
        1,
        "the rejected verification appended nothing"
    );
}

#[test]
#[cfg(unix)]
fn an_unreadable_hit_log_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::set_permissions(&log, fs::Permissions::from_mode(0o000)).expect("make the log unreadable");

    if fs::read_to_string(&log).is_ok() {
        // A user that ignores the file mode (root) cannot exercise this.
        return;
    }
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Io { .. })),
        "a permission error is surfaced, not read as an empty log"
    );
}
