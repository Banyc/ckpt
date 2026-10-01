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
    root.join("sessions").join(session.sparse_path())
}

fn flag_dir(root: &Path, session: &SessionId, flag: &FlagId) -> PathBuf {
    session_dir(root, session)
        .join("flags")
        .join(flag.sparse_path())
}

#[test]
fn a_session_lands_in_the_sparse_tree() {
    let (dir, store) = book();
    let session = store.session_new("perf plots").expect("session");

    let path = session.sparse_path();
    let shard = path.parent().expect("shard");
    let rest = path.file_name().expect("rest");
    assert_eq!(shard.as_os_str().len(), 2);
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
        format!("ab/{}/", "cd".repeat(19)),
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

    // The typed ids are what a command line parses; the bare object name is
    // flat only.
    assert_eq!(SessionId::parse(flat).expect("flat").as_id().as_str(), flat);
    assert_eq!(
        SessionId::parse(&sparse).expect("sparse").as_id().as_str(),
        flat
    );
    assert_eq!(
        SessionId::parse(&flat.to_uppercase())
            .expect("uppercase")
            .as_id()
            .as_str(),
        flat,
        "object names are stored lowercased"
    );
    assert_eq!(
        SessionId::parse(&format!(
            "{}/{}",
            flat[..2].to_uppercase(),
            flat[2..].to_uppercase()
        ))
        .expect("uppercase sparse")
        .as_id()
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

    fs::write(
        session_dir(dir.path(), &session)
            .parent()
            .expect("shard")
            .join(".DS_Store"),
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
    fs::create_dir(
        session_dir(dir.path(), &session)
            .parent()
            .expect("shard")
            .join("not-an-id"),
    )
    .expect("create");

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
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    // The flag's own log is the entry a rejected verification would have written
    // to, so the assertion is about that entry and not about some other flag.
    fs::write(&log, "{\"ts\":\"2030-01-01T00:00:00Z\"}").expect("write an unterminated line");

    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(&log).expect("read hits"),
        "{\"ts\":\"2030-01-01T00:00:00Z\"}",
        "the rejected verification left this flag's log as it found it"
    );
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
fn a_flag_under_a_session_without_meta_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_file(session_dir(dir.path(), &session).join("meta.json")).expect("remove meta");

    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "the mapping names a flag whose session is not a record"
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
fn a_file_at_a_flag_object_name_is_reported_by_the_session() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let stray = session_dir(dir.path(), &session)
        .join("flags")
        .join("ab")
        .join("0".repeat(38));
    fs::create_dir_all(stray.parent().expect("parent")).expect("create");
    fs::write(&stray, "not a record").expect("write stray");

    assert!(
        matches!(store.status(session.as_id()), Err(Error::Corrupt { .. })),
        "the session that holds the foreign entry reports it"
    );

    // The stray name was never mapped, so a lookup by that id finds no flag and
    // writes nothing into the damaged tree.
    let id = FlagId::parse(&format!("ab{}", "0".repeat(38))).expect("id");
    assert!(matches!(
        store.status(id.as_id()),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        store.verify(&id, &Verify::new()),
        Err(Error::NotFound { .. })
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
fn a_flag_copied_to_another_session_is_reported_there() {
    let (dir, store) = book();
    let first = store.session_new("first").expect("session");
    let second = store.session_new("second").expect("session");
    let flag = store.flag_new(&first, "flag").expect("flag");

    let from = flag_dir(dir.path(), &first, &flag);
    let to = flag_dir(dir.path(), &second, &flag);
    fs::create_dir_all(to.parent().expect("parent")).expect("create");
    copy_tree(&from, &to);

    assert!(
        store.verify(&flag, &Verify::new()).is_ok(),
        "the mapping names the owner, so the hit has one place to land"
    );
    assert!(
        store.status(first.as_id()).is_ok(),
        "the owner holds what the mapping gives it"
    );
    assert!(
        matches!(store.status(second.as_id()), Err(Error::Corrupt { .. })),
        "the session holding a flag the mapping does not give it is reported"
    );
    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
fn a_foreign_entry_in_a_flag_shard_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let shard = session_dir(dir.path(), &session).join("flags").join("ab");
    fs::create_dir_all(&shard).expect("create");
    fs::create_dir(shard.join("not-an-id")).expect("create foreign entry");

    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a flag lookup reports the damaged shard too"
    );
    assert!(
        matches!(
            store.verify(&flag, &Verify::new()),
            Err(Error::Corrupt { .. })
        ),
        "and a hit is not appended into a tree the reports call damaged"
    );
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
    let interrupted = store.session_new("session").expect("session");
    fs::remove_file(session_dir(dir.path(), &interrupted).join("meta.json")).expect("remove meta");

    assert!(
        store.sessions().expect("sessions").is_empty(),
        "a directory with no meta.json is an interrupted write, not a record"
    );
    assert!(
        matches!(
            store.status(interrupted.as_id()),
            Err(Error::NotFound { .. })
        ),
        "and it does not answer as a session either"
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
        store.status(session.as_id()).is_ok(),
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
    let rest = flag.sparse_path();
    let log = moved.join(rest.file_name().expect("rest")).join("hits.log");
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
        // A user that ignores the file mode (root) still reads it, so the log is
        // readable and the report says what it holds.
        assert!(
            store.status(flag.as_id()).is_ok(),
            "the log is readable for this user"
        );
        return;
    }
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Io { .. })),
        "a permission error is surfaced, not read as an empty log"
    );
}

#[test]
fn an_id_that_is_both_a_session_and_a_flag_is_reported() {
    let (dir, store) = book();
    let real = store.session_new("real").expect("session");
    let flag = store.flag_new(&real, "flag").expect("flag");
    let impostor = store.session_new("impostor").expect("session");

    // Move the second session to the flag's object name, so one id names both.
    let from = session_dir(dir.path(), &impostor);
    let to = session_dir(dir.path(), &SessionId::from_id(flag.as_id().clone()));
    fs::create_dir_all(to.parent().expect("parent")).expect("create");
    fs::rename(&from, &to).expect("move the impostor session");

    // Every entry point reports the collision rather than resolving one way.
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "a report of the id"
    );
    assert!(matches!(
        store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
#[cfg(unix)]
fn a_hard_link_at_a_hit_log_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");
    let linked = flag_dir(dir.path(), &session, &second).join("hits.log");
    fs::remove_file(&linked).expect("remove log");
    fs::hard_link(
        flag_dir(dir.path(), &session, &first).join("hits.log"),
        &linked,
    )
    .expect("hard link the two logs");

    assert!(
        matches!(
            store.verify(&second, &Verify::new()),
            Err(Error::Corrupt { .. })
        ),
        "a hit is not appended into a log shared with another flag"
    );
    assert_eq!(
        fs::read_to_string(flag_dir(dir.path(), &session, &first).join("hits.log"))
            .expect("read the other log"),
        "",
        "the other flag's log gained nothing"
    );
}

#[test]
fn the_mapping_is_the_session_and_nothing_else() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    let entry = dir.path().join("by-flag").join(flag.sparse_path());
    assert_eq!(
        fs::read_to_string(&entry).expect("read the mapping"),
        format!("{session}\n"),
        "the mapping is one session id, with no record field stored twice"
    );
}

#[test]
fn a_flag_the_mapping_does_not_name_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_file(dir.path().join("by-flag").join(flag.sparse_path())).expect("remove mapping");

    assert!(
        matches!(store.status(session.as_id()), Err(Error::Corrupt { .. })),
        "a report reports the flag it cannot place"
    );
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::NotFound { .. })),
        "a lookup by flag id has no mapping to follow"
    );
}

#[test]
fn a_mapping_whose_flag_is_gone_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_dir_all(flag_dir(dir.path(), &session, &flag)).expect("remove the flag");

    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "the mapping says a flag is there and the tree says otherwise"
    );
    assert!(
        store
            .status(session.as_id())
            .expect("status")
            .flags
            .is_empty(),
        "the session no longer holds it"
    );
}

#[test]
fn a_mapping_that_names_another_session_is_reported() {
    let (dir, store) = book();
    let first = store.session_new("first").expect("session");
    let second = store.session_new("second").expect("session");
    let flag = store.flag_new(&first, "flag").expect("flag");
    fs::write(
        dir.path().join("by-flag").join(flag.sparse_path()),
        format!("{second}\n"),
    )
    .expect("point the mapping at another session");

    assert!(
        matches!(store.status(first.as_id()), Err(Error::Corrupt { .. })),
        "the session holding a flag it is not mapped to is reported"
    );
    assert!(
        matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
        "the named session does not hold it"
    );
}

#[test]
fn a_mapping_that_is_not_a_session_id_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::write(
        dir.path().join("by-flag").join(flag.sparse_path()),
        "not an id\n",
    )
    .expect("write rubbish where the mapping belongs");

    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_corrupt_session_is_not_written_into_by_its_flags() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::write(
        session_dir(dir.path(), &session).join("meta.json"),
        "not json",
    )
    .expect("damage the session");

    assert!(
        matches!(
            store.verify(&flag, &Verify::new()),
            Err(Error::Corrupt { .. })
        ),
        "a hit is not appended into a session every report calls corrupt"
    );
    assert!(matches!(
        store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(&log).expect("read hits"),
        "",
        "nothing was written"
    );
}

#[test]
fn a_rejected_flag_addition_leaves_nothing_behind() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    fs::create_dir_all(session_dir(dir.path(), &session).join("flags").join("zz"))
        .expect("create a foreign shard");
    let before = fs::read_dir(session_dir(dir.path(), &session).join("flags"))
        .expect("list")
        .count();

    assert!(matches!(
        store.flag_new(&session, "flag"),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_dir(session_dir(dir.path(), &session).join("flags"))
            .expect("list")
            .count(),
        before,
        "the rejected addition created nothing"
    );
}

#[test]
fn a_hit_without_an_instant_is_recorded_now() {
    let (_, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");

    let before = Timestamp::now();
    let status = store.verify(&flag, &Verify::new()).expect("verify");
    let after = Timestamp::now();

    let recorded = status.last_hit.expect("the hit recorded an instant");
    assert!(
        before <= recorded && recorded <= after,
        "{recorded} is not between {before} and {after}"
    );
}

#[test]
fn a_note_with_quotes_and_newlines_stays_one_line() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    store
        .verify(
            &flag,
            &Verify {
                note: Some("first\nsecond \"quoted\"".to_owned()),
                at: at(1_000),
            },
        )
        .expect("verify");

    let text = fs::read_to_string(flag_dir(dir.path(), &session, &flag).join("hits.log"))
        .expect("read hits");
    assert_eq!(text.lines().count(), 1, "one line per hit, escapes and all");
    assert_eq!(
        store.status(flag.as_id()).expect("status").flags[0].hits,
        1,
        "and it reads back"
    );
}

#[test]
fn a_damaged_session_does_not_stop_another() {
    let (dir, store) = book();
    let good = store.session_new("good").expect("session");
    let bad = store.session_new("bad").expect("session");
    let flag = store.flag_new(&good, "flag").expect("flag");
    let spoiled = store.flag_new(&bad, "flag").expect("flag");
    fs::write(session_dir(dir.path(), &bad).join("meta.json"), "not json").expect("damage");

    assert!(
        store.verify(&flag, &Verify::new()).is_ok(),
        "a lookup does not walk the damaged session"
    );
    assert!(store.flag_status(&flag).is_ok());
    assert!(matches!(
        store.verify(&spoiled, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.status(bad.as_id()),
        Err(Error::Corrupt { .. })
    ));
    assert!(
        matches!(store.sessions(), Err(Error::Corrupt { .. })),
        "and the book as a whole still reports the damage"
    );
}

#[test]
fn a_lookup_agrees_with_a_report() {
    let (_, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let looked_up = store.flag_status(&flag).expect("flag status");
    let reported = store.status(flag.as_id()).expect("status");
    assert_eq!(looked_up.hits, reported.flags[0].hits);
    assert_eq!(looked_up.counter, reported.flags[0].counter);
    assert_eq!(looked_up.last_hit, reported.flags[0].last_hit);
}

#[test]
fn an_external_line_belongs_to_the_flag_that_holds_it() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");

    let log = flag_dir(dir.path(), &session, &second).join("hits.log");
    let mut text = fs::read_to_string(&log).expect("read the log");
    text.push_str("{\"ts\":\"2030-01-01T00:00:00Z\"}\n");
    fs::write(&log, text).expect("append out of band");

    assert_eq!(store.flag_status(&second).expect("second").hits, 1);
    assert_eq!(
        store.flag_status(&first).expect("first").hits,
        0,
        "the sibling is untouched"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_session_is_not_read_or_written_through() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let outside = TempDir::new().expect("temp dir");
    let link = session_dir(dir.path(), &session);
    let moved = outside.path().join("session");
    fs::rename(&link, &moved).expect("move the session out of the tree");
    std::os::unix::fs::symlink(&moved, &link).expect("symlink");

    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a link at a session is refused by a lookup"
    );
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
    let log = moved
        .join("flags")
        .join(flag.sparse_path())
        .join("hits.log");
    assert_eq!(
        fs::read_to_string(&log)
            .expect("read the log")
            .lines()
            .count(),
        1,
        "no hit was appended outside the store"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_session_shard_is_not_read_or_written_through() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    verify_at(&store, &flag, 1_000);

    let outside = TempDir::new().expect("temp dir");
    let session_path = session_dir(dir.path(), &session);
    let shard = session_path.parent().expect("shard").to_path_buf();
    let rest = session_path
        .strip_prefix(&shard)
        .expect("the session inside its shard")
        .to_path_buf();
    let moved = outside.path().join("shard");
    fs::rename(&shard, &moved).expect("move the shard out of the tree");
    std::os::unix::fs::symlink(&moved, &shard).expect("symlink");

    assert!(matches!(
        store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
    let log = moved
        .join(rest)
        .join("flags")
        .join(flag.sparse_path())
        .join("hits.log");
    assert_eq!(
        fs::read_to_string(&log)
            .expect("read the log")
            .lines()
            .count(),
        1,
        "no hit was appended outside the store"
    );
}

#[test]
fn a_mapping_that_is_not_a_file_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let entry = dir.path().join("by-flag").join(flag.sparse_path());
    fs::remove_file(&entry).expect("remove the mapping");
    fs::create_dir(&entry).expect("put a directory where the mapping belongs");

    assert!(matches!(
        store.status(flag.as_id()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_flag_is_not_added_through_a_symlinked_flags_directory() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let outside = TempDir::new().expect("temp dir");
    let flags = session_dir(dir.path(), &session).join("flags");
    let moved = outside.path().join("flags");
    fs::rename(&flags, &moved).expect("move flags out of the tree");
    std::os::unix::fs::symlink(&moved, &flags).expect("symlink");

    assert!(matches!(
        store.flag_new(&session, "flag"),
        Err(Error::Corrupt { .. })
    ));
    assert!(
        moved
            .read_dir()
            .expect("read the moved flags")
            .next()
            .is_none(),
        "nothing was created outside the store"
    );
}

#[test]
fn a_report_orders_a_full_tie_by_object_name() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");

    // The same instant and the same counter: only the object names are left.
    let instant = Timestamp::from_second(1_000).expect("instant").to_string();
    for flag in [&first, &second] {
        let path = flag_dir(dir.path(), &session, flag).join("meta.json");
        let text = fs::read_to_string(&path).expect("read meta");
        let mut value: serde_json::Value = serde_json::from_str(&text).expect("json");
        value["created"] = serde_json::Value::String(instant.clone());
        value["counter"] = serde_json::Value::from(1);
        fs::write(&path, format!("{value}\n")).expect("write meta");
    }

    let status = store.status(session.as_id()).expect("status");
    let order: Vec<FlagId> = status.flags.iter().map(|flag| flag.id.clone()).collect();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(order, expected, "a full tie is settled by object name");
}

#[test]
fn a_damaged_sibling_flag_is_seen_by_a_lookup() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");
    let log = flag_dir(dir.path(), &session, &first).join("hits.log");
    fs::write(
        flag_dir(dir.path(), &session, &second).join("meta.json"),
        "not json",
    )
    .expect("damage the sibling");

    assert!(
        matches!(store.flag_status(&first), Err(Error::Corrupt { .. })),
        "a lookup reads the owner's flags the way a report does"
    );
    assert!(matches!(
        store.verify(&first, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
    assert_eq!(
        fs::read_to_string(&log).expect("read the log"),
        "",
        "nothing was appended to a session that reads as damaged"
    );
}

#[test]
fn a_damaged_sibling_log_is_seen_by_a_lookup() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let first = store.flag_new(&session, "first").expect("flag");
    let second = store.flag_new(&session, "second").expect("flag");
    fs::write(
        flag_dir(dir.path(), &session, &second).join("hits.log"),
        "not json\n",
    )
    .expect("damage the sibling's log");

    assert!(matches!(
        store.verify(&first, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn a_flag_meta_that_does_not_parse_stops_a_verification() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");
    fs::write(
        flag_dir(dir.path(), &session, &flag).join("meta.json"),
        "not json",
    )
    .expect("damage the flag");

    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(&log).expect("read the log"),
        "",
        "the rejected verification appended nothing"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_sessions_directory_is_reported_by_a_lookup() {
    // A complete store elsewhere, reached through a link at this store's
    // sessions directory: the second directory level is what refuses it.
    let (real_dir, real_store) = book();
    let session = real_store.session_new("session").expect("session");
    let flag = real_store.flag_new(&session, "flag").expect("flag");

    let (linked_dir, linked_store) = book();
    std::os::unix::fs::symlink(
        real_dir.path().join("sessions"),
        linked_dir.path().join("sessions"),
    )
    .expect("symlink the sessions directory");
    // The mapping is there, so the lookup reaches the sessions tree, and what
    // refuses it is the link two levels up.
    let entry = linked_dir.path().join("by-flag").join(flag.sparse_path());
    fs::create_dir_all(entry.parent().expect("parent")).expect("create");
    fs::write(&entry, format!("{session}\n")).expect("write the mapping");

    assert!(
        matches!(linked_store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a link at sessions is refused two levels up"
    );
    assert!(matches!(
        linked_store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_to_string(flag_dir(real_dir.path(), &session, &flag).join("hits.log"))
            .expect("read the real log"),
        "",
        "nothing was written through it"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_mapping_directory_is_reported_by_a_lookup() {
    let (real_dir, real_store) = book();
    let session = real_store.session_new("session").expect("session");
    let flag = real_store.flag_new(&session, "flag").expect("flag");

    let (linked_dir, linked_store) = book();
    fs::create_dir_all(linked_dir.path()).expect("root");
    std::os::unix::fs::symlink(
        real_dir.path().join("by-flag"),
        linked_dir.path().join("by-flag"),
    )
    .expect("symlink the mapping directory");

    assert!(matches!(
        linked_store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        linked_store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn an_addition_refuses_an_id_that_is_both() {
    let (dir, store) = book();
    let real = store.session_new("real").expect("session");
    let flag = store.flag_new(&real, "flag").expect("flag");
    let impostor = store.session_new("impostor").expect("session");

    let from = session_dir(dir.path(), &impostor);
    let to = session_dir(dir.path(), &SessionId::from_id(flag.as_id().clone()));
    fs::create_dir_all(to.parent().expect("parent")).expect("create");
    fs::rename(&from, &to).expect("move the impostor session");

    assert!(
        matches!(
            store.flag_new(&SessionId::from_id(flag.as_id().clone()), "another"),
            Err(Error::Corrupt { .. })
        ),
        "an addition applies the same rule as a listing"
    );
}

#[test]
fn a_session_whose_id_is_mapped_is_reported_by_an_addition() {
    let (dir, store) = book();
    let real = store.session_new("real").expect("session");
    store.flag_new(&real, "flag").expect("flag");
    let impostor = store.session_new("impostor").expect("session");

    // A mapping entry for the impostor's own id, naming another session.
    let entry = dir.path().join("by-flag").join(impostor.sparse_path());
    fs::create_dir_all(entry.parent().expect("parent")).expect("create");
    fs::write(&entry, format!("{real}\n")).expect("write the mapping");

    assert!(matches!(
        store.flag_new(&impostor, "flag"),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn an_unmapped_flag_id_that_is_also_a_session_is_a_session() {
    let (dir, store) = book();
    let real = store.session_new("real").expect("session");
    let flag = store.flag_new(&real, "flag").expect("flag");
    let impostor = store.session_new("impostor").expect("session");

    let from = session_dir(dir.path(), &impostor);
    let to = session_dir(dir.path(), &SessionId::from_id(flag.as_id().clone()));
    fs::create_dir_all(to.parent().expect("parent")).expect("create");
    fs::rename(&from, &to).expect("move the impostor session");
    fs::remove_file(dir.path().join("by-flag").join(flag.sparse_path()))
        .expect("remove the mapping");

    // The mapping decides whether an id is a flag, so with it gone this is a
    // session; the flag it also names is reported by the walk that lists it.
    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(status.session.as_id(), flag.as_id());
    assert!(matches!(store.sessions(), Err(Error::Corrupt { .. })));
}

#[test]
fn a_sparse_split_that_is_not_two_and_thirty_eight_is_rejected() {
    let flat = "1".repeat(40);
    let canonical = format!("{}/{}", &flat[..2], &flat[2..]);
    assert!(
        SessionId::parse(&canonical).is_ok(),
        "the canonical split is the sparse form"
    );

    for bad in [
        format!("0/{}", "1".repeat(39)),
        format!("{}/{}", "0".repeat(3), "1".repeat(37)),
        format!("ab/{}/", "cd".repeat(19)),
    ] {
        assert!(
            matches!(SessionId::parse(&bad), Err(Error::InvalidId { .. })),
            "SessionId::parse({bad})"
        );
        assert!(
            matches!(FlagId::parse(&bad), Err(Error::InvalidId { .. })),
            "FlagId::parse({bad})"
        );
    }
}

#[test]
fn a_mapping_shard_that_is_not_a_directory_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let shard = dir
        .path()
        .join("by-flag")
        .join(flag.sparse_path())
        .parent()
        .expect("shard")
        .to_path_buf();
    fs::remove_dir_all(&shard).expect("remove the mapping shard");
    fs::write(&shard, "not a directory").expect("put a file where the shard belongs");

    assert!(matches!(
        store.flag_status(&flag),
        Err(Error::Corrupt { .. })
    ));
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn an_unknown_field_in_a_record_is_read_past() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::write(
        flag_dir(dir.path(), &session, &flag).join("hits.log"),
        "{\"ts\":\"2030-01-01T00:00:00Z\",\"extra\":true}\n",
    )
    .expect("write a hit with a field this version does not know");

    let status = store.status(flag.as_id()).expect("status");
    assert_eq!(
        status.flags[0].hits, 1,
        "the fields this version knows are read, the rest are passed over"
    );

    // The same for a record of the book itself.
    let session_meta = session_dir(dir.path(), &session).join("meta.json");
    fs::write(
        &session_meta,
        "{\"desc\":\"session\",\"created\":\"2030-01-01T00:00:00Z\",\"extra\":1}\n",
    )
    .expect("write a session record with an unknown field");
    assert_eq!(
        store.status(session.as_id()).expect("status").desc,
        "session"
    );
}

#[test]
#[cfg(unix)]
fn a_foreign_session_directory_is_reported_even_with_no_shard_below_it() {
    // An empty root with a link at sessions: nothing is there yet, and the link
    // is reported rather than answered as "no such id".
    let (dir, store) = book();
    let outside = TempDir::new().expect("temp dir");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("sessions")).expect("symlink");

    let absent = SessionId::generate();
    assert!(
        matches!(store.status(absent.as_id()), Err(Error::Corrupt { .. })),
        "the link is reported, not hidden behind the missing shard"
    );
}

#[test]
#[cfg(unix)]
fn a_foreign_mapping_directory_is_reported_even_with_no_shard_below_it() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::remove_file(dir.path().join("by-flag").join(flag.sparse_path())).expect("remove the entry");
    fs::remove_dir_all(dir.path().join("by-flag")).expect("remove the mapping directory");
    let outside = TempDir::new().expect("temp dir");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("by-flag")).expect("symlink");

    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a lookup reports the link in the mapping directory"
    );
}

#[test]
fn a_mapping_naming_a_session_that_holds_other_flags_is_reported() {
    let (dir, store) = book();
    let first = store.session_new("first").expect("session");
    let second = store.session_new("second").expect("session");
    let wanted = store.flag_new(&first, "wanted").expect("flag");
    store.flag_new(&first, "sibling").expect("flag");
    store.flag_new(&second, "other").expect("flag");

    // The mapping points at a session that does hold flags, just not this one.
    fs::write(
        dir.path().join("by-flag").join(wanted.sparse_path()),
        format!("{second}\n"),
    )
    .expect("point the mapping at the other session");

    assert!(
        matches!(store.flag_status(&wanted), Err(Error::Corrupt { .. })),
        "the named session does not hold this flag, whatever else it holds"
    );
    assert!(matches!(
        store.status(wanted.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_hard_link_at_a_meta_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let meta = flag_dir(dir.path(), &session, &flag).join("meta.json");
    fs::hard_link(&meta, dir.path().join("elsewhere.json")).expect("hard link the meta");

    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a record file shared with another name is refused"
    );
    assert!(matches!(
        store.status(session.as_id()),
        Err(Error::Corrupt { .. })
    ));

    // The same for a session's own record.
    let other = store.session_new("other").expect("session");
    let meta = session_dir(dir.path(), &other).join("meta.json");
    fs::hard_link(&meta, dir.path().join("elsewhere2.json")).expect("hard link the session meta");
    assert!(matches!(
        store.status(other.as_id()),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
#[cfg(unix)]
fn a_rejected_addition_leaves_no_directory_behind() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let outside = TempDir::new().expect("temp dir");
    // A link where the mapping belongs, which the addition runs into first.
    std::os::unix::fs::symlink(outside.path(), dir.path().join("by-flag")).expect("symlink");
    let flags = session_dir(dir.path(), &session).join("flags");

    assert!(matches!(
        store.flag_new(&session, "flag"),
        Err(Error::Corrupt { .. })
    ));
    assert_eq!(
        fs::read_dir(&flags).expect("list").count(),
        0,
        "the rejected addition created no record directory"
    );
    assert!(
        outside
            .path()
            .read_dir()
            .expect("read outside")
            .next()
            .is_none(),
        "and nothing outside the store"
    );
}

fn count_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            if entry.path().is_dir() {
                count_files(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

#[test]
#[cfg(unix)]
fn a_failed_addition_takes_its_mapping_back() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flags = session_dir(dir.path(), &session).join("flags");
    fs::set_permissions(&flags, fs::Permissions::from_mode(0o500)).expect("make flags read-only");

    let added = store.flag_new(&session, "flag");
    fs::set_permissions(&flags, fs::Permissions::from_mode(0o700)).expect("restore");

    if added.is_ok() {
        // A user that ignores the file mode (root) cannot fail this way.
        assert_eq!(
            store.status(session.as_id()).expect("status").flags.len(),
            1
        );
        return;
    }
    assert_eq!(
        count_files(&dir.path().join("by-flag")),
        0,
        "the mapping entry the failed addition wrote is gone, so no id is left mapped"
    );
    assert!(
        store.sessions().is_ok(),
        "whatever the failed addition left is not reported as damage"
    );
    assert!(
        store
            .status(session.as_id())
            .expect("status")
            .flags
            .is_empty(),
        "and no flag is listed"
    );
}

#[test]
#[cfg(unix)]
fn a_hard_link_at_a_mapping_entry_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let entry = dir.path().join("by-flag").join(flag.sparse_path());
    fs::hard_link(&entry, dir.path().join("elsewhere")).expect("hard link the mapping");

    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a mapping entry shared with another name is refused"
    );
}

#[test]
fn a_mapping_entry_holding_more_than_an_id_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::write(
        dir.path().join("by-flag").join(flag.sparse_path()),
        format!("  {session}\n"),
    )
    .expect("pad the mapping");

    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "the mapping holds one id, not a field to search through"
    );
}

#[test]
fn a_mapping_entry_may_be_written_in_sparse_form() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    fs::write(
        dir.path().join("by-flag").join(flag.sparse_path()),
        format!(
            "{}/{}\n",
            &session.as_id().as_str()[..2],
            &session.as_id().as_str()[2..]
        ),
    )
    .expect("write the mapping in the sparse form");

    assert_eq!(
        store.flag_status(&flag).expect("flag status").id,
        flag,
        "a sparse session id is read the way a flat one is"
    );
}

#[test]
fn a_padded_hit_line_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let log = flag_dir(dir.path(), &session, &flag).join("hits.log");

    for line in [
        "{\"ts\":\"2030-01-01T00:00:00Z\"}\r\n",
        "  {\"ts\":\"2030-01-01T00:00:00Z\"}\n",
        "{\"ts\":\"2030-01-01T00:00:00Z\"} \n",
    ] {
        fs::write(&log, line).expect("write a padded line");
        assert!(
            matches!(store.status(flag.as_id()), Err(Error::Corrupt { .. })),
            "the writer emits one JSON object and a newline, so {line:?} is damage"
        );
    }
}

#[test]
#[cfg(unix)]
fn a_symlink_at_a_mapping_entry_is_reported() {
    let (dir, store) = book();
    let session = store.session_new("session").expect("session");
    let flag = store.flag_new(&session, "flag").expect("flag");
    let entry = dir.path().join("by-flag").join(flag.sparse_path());
    let outside = TempDir::new().expect("temp dir");
    let target = outside.path().join("entry");
    fs::write(&target, format!("{session}\n")).expect("write the target");
    fs::remove_file(&entry).expect("remove the mapping");
    std::os::unix::fs::symlink(&target, &entry).expect("symlink the mapping");

    assert!(
        matches!(store.flag_status(&flag), Err(Error::Corrupt { .. })),
        "a mapping entry that is a link is refused"
    );
    assert!(matches!(
        store.verify(&flag, &Verify::new()),
        Err(Error::Corrupt { .. })
    ));
}
