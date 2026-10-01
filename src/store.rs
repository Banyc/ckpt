//! The record book as a directory tree.
//!
//! ```text
//! <root>/sessions/<a>/<b>/meta.json
//!                        /flags/<a>/<b>/meta.json
//!                                     /hits.log
//! ```
//!
//! A session exists once its `meta.json` is there; a ctf flag exists once its
//! own `meta.json` is. A `hits.log` holds one JSON line per verification, so a
//! count is a line count and an append is a single atomic write.
//!
//! Every level the store owns is checked for what it is before it is used: the
//! `sessions` directory, each shard, each record directory, and the record files
//! themselves. A missing record is absent, a real directory with a regular
//! `meta.json` is a record, and anything else — a file, a symlink, or a
//! `meta.json` that is not a regular file — is reported as a foreign entry
//! rather than read or written through.
//!
//! A directory at an object name with no regular `meta.json` is an interrupted
//! write rather than a record, and is treated as absent: a partial write must
//! not poison a record book that has no cleanup command.
//!
//! A record directory is read by name — its `meta.json` and `hits.log` — so
//! other names inside it are ignored. The checks above are made before a path is
//! used rather than while it is open: they refuse links and foreign entries left
//! in the tree, they do not defend against another process swapping one in
//! mid-operation, because the store belongs to the process reading it.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::Error;
use crate::id::{FlagId, HEX_LEN, Id, SHARD_LEN, SessionId};
use crate::record::{FlagMeta, FlagStatus, Hit, SessionMeta, SessionSummary, Status, Target};

/// Settings for [`Store::verify`].
#[derive(Clone, Debug)]
pub struct Verify {
    /// Note to store with the hit.
    pub note: Option<String>,
    /// Instant to record; the current instant when unset.
    pub at: Option<Timestamp>,
}

impl Verify {
    /// The current instant, with no note.
    pub fn new() -> Verify {
        Verify {
            note: None,
            at: None,
        }
    }
}

impl Default for Verify {
    fn default() -> Verify {
        Verify::new()
    }
}

/// A record book rooted at one directory.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// The store at `$CKPT_ROOT`, falling back to `$TMPDIR/ckpt`.
    pub fn from_env() -> Store {
        match std::env::var_os("CKPT_ROOT") {
            Some(root) if !root.is_empty() => Store::at(root),
            _ => Store::at(std::env::temp_dir().join("ckpt")),
        }
    }

    /// The store rooted at `root`.
    pub fn at(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// The directory every session lives under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create a session and return its id.
    ///
    /// The root itself is created as needed and may be a link the caller chose;
    /// every directory below it is created one level at a time so a link
    /// planted above a record is reported instead of followed.
    pub fn session_new(&self, desc: &str) -> Result<SessionId, Error> {
        let session = SessionId::generate();
        fs::create_dir_all(&self.root).map_err(|err| Error::io(&self.root, err))?;
        ensure_directory(&self.sessions_dir())?;
        ensure_directory(&self.session_shard_dir(&session))?;
        let dir = self.session_dir(&session);
        create_record_dir(&dir)?;
        ensure_directory(&self.flags_dir(&session))?;
        write_record(&meta_path(&dir), &new_session_meta(desc))?;
        Ok(session)
    }

    /// Add a ctf flag to `session` and return its id.
    pub fn flag_new(&self, session: &SessionId, desc: &str) -> Result<FlagId, Error> {
        let dir = self.session_dir(session);
        if !holds_record(&dir)? {
            return Err(Error::NotFound {
                kind: "session",
                id: session.to_string(),
            });
        }
        // The session's own `meta.json` must parse: a flag is not added to a
        // record that already reads as corrupt.
        read_session_meta(&dir)?;
        // The `flags/` entry belongs to the session. A missing or foreign one
        // is reported rather than created here, so a write never heals a
        // damaged tree behind the read paths' back.
        require_directory(&self.flags_dir(session))?;

        let flag = FlagId::generate();
        let flag_dir = self.flag_dir(session, &flag);
        ensure_directory(&self.flag_shard_dir(session, &flag))?;
        create_record_dir(&flag_dir)?;
        let hits = hits_path(&flag_dir);
        File::create(&hits).map_err(|err| Error::io(&hits, err))?;
        let counter = self.flag_ids(session)?.len() as u64 + 1;
        write_record(
            &meta_path(&flag_dir),
            &FlagMeta {
                desc: desc.to_owned(),
                created: Timestamp::now(),
                counter,
            },
        )?;
        Ok(flag)
    }

    /// Append one verification hit for `flag` and report its count.
    ///
    /// The count is the number of hits the log held when this append was
    /// prepared, plus this one. A concurrent append can land first, so the
    /// number is a lower bound on the log's total; [`Store::status`] is the
    /// authority on the total.
    ///
    /// An `Ok` result means one complete hit line was appended. An error means
    /// no complete hit was appended: a damaged or foreign record is rejected
    /// with the log untouched, while a short write can leave a partial line,
    /// which the next read reports as damage.
    pub fn verify(&self, flag: &FlagId, settings: &Verify) -> Result<FlagStatus, Error> {
        let session = self.flag_owner(flag)?.ok_or_else(|| Error::NotFound {
            kind: "flag",
            id: flag.to_string(),
        })?;
        let dir = self.flag_dir(&session, flag);

        // Read before writing: a damaged log is reported without a hit having
        // been appended.
        let meta = read_flag_meta(&dir)?;
        let hits = read_hits(&hits_path(&dir))?.len() as u64 + 1;
        let hit = Hit {
            ts: settings.at.unwrap_or_else(Timestamp::now),
            note: settings.note.clone(),
        };
        append_hit(&hits_path(&dir), &hit)?;

        Ok(FlagStatus {
            id: flag.clone(),
            desc: meta.desc,
            created: meta.created,
            counter: meta.counter,
            hits,
            last_hit: Some(hit.ts),
        })
    }

    /// The status of the session `id` names, where `id` may be a session id or
    /// the id of any of that session's ctf flags.
    pub fn status(&self, id: &Id) -> Result<Status, Error> {
        let session = SessionId::from_id(id.clone());
        if holds_record(&self.session_dir(&session))? {
            return self.session_status(&session, Target::Session);
        }
        let flag = FlagId::from_id(id.clone());
        let owner = self.flag_owner(&flag)?.ok_or_else(|| Error::NotFound {
            kind: "session or flag",
            id: id.to_string(),
        })?;
        self.session_status(&owner, Target::Flag { id: flag })
    }

    /// Every session in the record book, ordered by id.
    pub fn sessions(&self) -> Result<Vec<SessionSummary>, Error> {
        let mut summaries = Vec::new();
        for session in self.session_ids()? {
            let flags = self.flag_statuses(&session)?;
            let meta = read_session_meta(&self.session_dir(&session))?;
            summaries.push(SessionSummary {
                session,
                desc: meta.desc,
                created: meta.created,
                flags: flags.len() as u64,
                hits: flags.iter().map(|flag| flag.hits).sum(),
            });
        }
        Ok(summaries)
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn session_shard_dir(&self, session: &SessionId) -> PathBuf {
        self.sessions_dir().join(session.as_id().sparse().0)
    }

    fn session_dir(&self, session: &SessionId) -> PathBuf {
        self.sessions_dir().join(session.sparse_path())
    }

    fn flags_dir(&self, session: &SessionId) -> PathBuf {
        self.session_dir(session).join("flags")
    }

    fn flag_shard_dir(&self, session: &SessionId, flag: &FlagId) -> PathBuf {
        self.flags_dir(session).join(flag.as_id().sparse().0)
    }

    fn flag_dir(&self, session: &SessionId, flag: &FlagId) -> PathBuf {
        self.flags_dir(session).join(flag.sparse_path())
    }

    fn session_ids(&self) -> Result<Vec<SessionId>, Error> {
        let mut ids = Vec::new();
        for (shard, shard_path) in owned_entries(&self.sessions_dir(), Missing::Empty)? {
            require_shard(&shard, &shard_path)?;
            for (rest, rest_path) in owned_entries(&shard_path, Missing::Empty)? {
                let session = SessionId::from_id(sparse_id(&shard, &rest, &rest_path)?);
                if holds_record(&rest_path)? {
                    ids.push(session);
                }
            }
        }
        Ok(ids)
    }

    fn flag_ids(&self, session: &SessionId) -> Result<Vec<FlagId>, Error> {
        let mut ids = Vec::new();
        for (shard, shard_path) in owned_entries(&self.flags_dir(session), Missing::Error)? {
            require_shard(&shard, &shard_path)?;
            for (rest, rest_path) in owned_entries(&shard_path, Missing::Error)? {
                let flag = FlagId::from_id(sparse_id(&shard, &rest, &rest_path)?);
                if holds_record(&rest_path)? {
                    ids.push(flag);
                }
            }
        }
        Ok(ids)
    }

    /// The one session a ctf flag is recorded under.
    ///
    /// A flag is stored inside its session, so this walks the `flags/` entry of
    /// every session: the tree is the index, and nothing is cached. The same
    /// flag id recorded under two sessions is reported rather than resolved to
    /// one of them, because a hit would otherwise land in whichever the walk
    /// reached first.
    fn flag_owner(&self, flag: &FlagId) -> Result<Option<SessionId>, Error> {
        let mut owner: Option<SessionId> = None;
        for session in self.session_ids()? {
            // The session exists, so its `flags/` entry must too: a flag lookup
            // reports a damaged tree instead of answering "no such flag".
            require_directory(&self.flags_dir(&session))?;
            let dir = self.flag_dir(&session, flag);
            if !holds_record(&dir)? {
                continue;
            }
            if owner.is_some() {
                return Err(Error::Corrupt {
                    path: dir,
                    detail: format!("the flag `{flag}` is recorded under two sessions"),
                });
            }
            owner = Some(session);
        }
        Ok(owner)
    }

    fn flag_status(&self, session: &SessionId, flag: &FlagId) -> Result<FlagStatus, Error> {
        let dir = self.flag_dir(session, flag);
        let meta = read_flag_meta(&dir)?;
        let hits = read_hits(&hits_path(&dir))?;
        Ok(FlagStatus {
            id: flag.clone(),
            desc: meta.desc,
            created: meta.created,
            counter: meta.counter,
            hits: hits.len() as u64,
            last_hit: hits.last().map(|hit| hit.ts),
        })
    }

    /// A session's flags in the order they were added: by timestamp, and by the
    /// counter where two timestamps are equal.
    fn flag_statuses(&self, session: &SessionId) -> Result<Vec<FlagStatus>, Error> {
        let mut statuses: Vec<FlagStatus> = self
            .flag_ids(session)?
            .iter()
            .map(|flag| self.flag_status(session, flag))
            .collect::<Result<_, _>>()?;
        statuses.sort_by(|left, right| {
            (left.created, left.counter).cmp(&(right.created, right.counter))
        });
        Ok(statuses)
    }

    fn session_status(&self, session: &SessionId, matched: Target) -> Result<Status, Error> {
        let meta = read_session_meta(&self.session_dir(session))?;
        Ok(Status {
            session: session.clone(),
            desc: meta.desc,
            created: meta.created,
            matched,
            flags: self.flag_statuses(session)?,
        })
    }
}

/// What an absent directory means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Missing {
    /// Absent and empty are the same thing: the store root before its first
    /// session.
    Empty,
    /// Absent is a broken record: the directory belongs to a session that is
    /// already there.
    Error,
}

/// The non-dot entries of a directory the store owns, sorted by name.
///
/// Dotfiles belong to the operating system and are skipped; every other entry
/// must be an object name. A path that is not a real directory is a foreign
/// entry, whether it is a file, a symlink to a directory, or a dangling
/// symlink.
fn owned_entries(dir: &Path, missing: Missing) -> Result<Vec<(String, PathBuf)>, Error> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(Error::Corrupt {
                    path: dir.to_path_buf(),
                    detail: "a store directory must be a real directory".to_owned(),
                });
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if missing == Missing::Empty {
                return Ok(Vec::new());
            }
        }
        Err(err) => return Err(Error::io(dir, err)),
    }

    let entries = fs::read_dir(dir).map_err(|err| Error::io(dir, err))?;
    let mut owned = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| Error::io(dir, err))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Err(Error::Corrupt {
                path: entry.path(),
                detail: "file name is not valid UTF-8".to_owned(),
            });
        };
        if name.starts_with('.') {
            continue;
        }
        owned.push((name, entry.path()));
    }
    owned.sort();
    Ok(owned)
}

/// Whether a directory holds a record.
///
/// A missing directory holds nothing. A real directory with a regular
/// `meta.json` holds one. A directory without a `meta.json` is the remains of an
/// interrupted write, and is absent too, so a partial write cannot poison a
/// record book that has no cleanup command. A file or symlink at the object
/// name, a shard above it that is not a real directory, or a `meta.json` that is
/// not a regular file is a foreign entry: it is reported rather than read as an
/// absent record, so a damaged tree cannot answer "no such session".
fn holds_record(dir: &Path) -> Result<bool, Error> {
    if !ancestors_are_real(dir)? {
        return Ok(false);
    }
    match fs::symlink_metadata(dir) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::io(dir, err)),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(Error::Corrupt {
                path: dir.to_path_buf(),
                detail: "an object name must be a directory".to_owned(),
            });
        }
        Ok(_) => {}
    }

    let meta = meta_path(dir);
    match fs::symlink_metadata(&meta) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(Error::io(&meta, err)),
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(Error::Corrupt {
            path: meta,
            detail: "meta.json must be a regular file".to_owned(),
        }),
    }
}

/// Whether the two directories directly above a record are real directories.
///
/// `Ok(false)` means one of them is absent, so the record it would hold is
/// absent too. A file or a symlink in their place is a foreign entry: it is
/// reported rather than followed, so a link planted above a record cannot make
/// a write land outside the store.
fn ancestors_are_real(dir: &Path) -> Result<bool, Error> {
    for ancestor in dir.ancestors().skip(1).take(2) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Error::Corrupt {
                    path: ancestor.to_path_buf(),
                    detail: "a store directory must be a real directory".to_owned(),
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(Error::io(ancestor, err)),
        }
    }
    Ok(true)
}

/// Report a path that must be a real directory, like a session's `flags/`.
fn require_directory(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|err| Error::io(path, err))?;
    if metadata.is_dir() {
        return Ok(());
    }
    Err(Error::Corrupt {
        path: path.to_path_buf(),
        detail: "a store directory must be a real directory".to_owned(),
    })
}

/// Create a directory the store owns, or report a foreign entry in its place.
///
/// `create_dir` does not follow a link at the final component, so an existing
/// link fails with `AlreadyExists` and is checked rather than used.
fn ensure_directory(path: &Path) -> Result<(), Error> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => require_directory(path),
        Err(err) => Err(Error::io(path, err)),
    }
}

/// Create a fresh record directory, which must not already exist.
///
/// A shard directory may be shared by many records, so it is created tolerantly;
/// a record directory belongs to one id, and reusing the name would overwrite
/// the record already there.
fn create_record_dir(path: &Path) -> Result<(), Error> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(Error::io(
            path,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an object name already holds a record",
            ),
        )),
        Err(err) => Err(Error::io(path, err)),
    }
}

/// Report a record file that is not a regular file.
///
/// A link at `meta.json` or `hits.log` would let a read or an append land
/// somewhere else in the filesystem.
fn require_regular_file(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: "a record file must be a regular file".to_owned(),
        }),
        Err(err) => Err(Error::io(path, err)),
    }
}

/// Report a shard directory whose name is not exactly two lowercase-hex
/// characters.
///
/// A shard is checked before its contents, so a foreign shard is reported even
/// when it is empty and no object name is ever reassembled from it.
fn require_shard(shard: &str, path: &Path) -> Result<(), Error> {
    if shard.len() == SHARD_LEN && is_lowercase_hex(shard) {
        return Ok(());
    }
    Err(Error::Corrupt {
        path: path.to_path_buf(),
        detail: format!("`{shard}` is not a {SHARD_LEN}-character lowercase hex object name"),
    })
}

/// Reassemble the object name a sparse directory pair spells out.
///
/// The layout is exactly two lowercase-hex characters over the remaining 38.
/// Any other split or case is a foreign entry rather than a second spelling of
/// an id that is already stored, and is reported as such.
fn sparse_id(shard: &str, rest: &str, path: &Path) -> Result<Id, Error> {
    let canonical = shard.len() == SHARD_LEN
        && rest.len() == HEX_LEN - SHARD_LEN
        && is_lowercase_hex(shard)
        && is_lowercase_hex(rest);
    if !canonical {
        return Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: format!(
                "`{shard}/{rest}` is not a {SHARD_LEN}/{}-character lowercase hex object name",
                HEX_LEN - SHARD_LEN
            ),
        });
    }
    Id::parse(&format!("{shard}{rest}")).map_err(|_| Error::Corrupt {
        path: path.to_path_buf(),
        detail: format!("`{shard}/{rest}` is not an object name"),
    })
}

fn is_lowercase_hex(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.json")
}

fn hits_path(dir: &Path) -> PathBuf {
    dir.join("hits.log")
}

fn read_session_meta(dir: &Path) -> Result<SessionMeta, Error> {
    read_record(&meta_path(dir))
}

fn read_flag_meta(dir: &Path) -> Result<FlagMeta, Error> {
    read_record(&meta_path(dir))
}

fn new_session_meta(desc: &str) -> SessionMeta {
    SessionMeta {
        desc: desc.to_owned(),
        created: Timestamp::now(),
    }
}

fn read_record<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    require_regular_file(path)?;
    let text = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    serde_json::from_str(&text).map_err(|err| Error::Corrupt {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })
}

fn write_record<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(Error::Corrupt {
                path: path.to_path_buf(),
                detail: "a record file must be a regular file".to_owned(),
            });
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(Error::io(path, err)),
    }
    let text = serde_json::to_string(value).map_err(|err| Error::Corrupt {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })?;
    fs::write(path, format!("{text}\n")).map_err(|err| Error::io(path, err))
}

/// The hits in a log.
///
/// A blank or unparseable line is a damaged record: the writer only ever appends
/// a complete JSON object followed by a newline, so anything else is reported
/// rather than counted or skipped.
fn read_hits(path: &Path) -> Result<Vec<Hit>, Error> {
    require_regular_file(path)?;
    let text = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: "the final line has no terminating newline".to_owned(),
        });
    }
    text.lines()
        .map(|line| {
            serde_json::from_str(line).map_err(|err| Error::Corrupt {
                path: path.to_path_buf(),
                detail: err.to_string(),
            })
        })
        .collect()
}

/// Append one hit as a single complete line.
///
/// The whole line is built in one buffer and handed to one `write` call, and
/// `O_APPEND` makes the offset update and that write atomic, so two concurrent
/// verifications cannot interleave halves of a line.
fn append_hit(path: &Path, hit: &Hit) -> Result<(), Error> {
    require_regular_file(path)?;
    let mut line = serde_json::to_string(hit).map_err(|err| Error::Corrupt {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|err| Error::io(path, err))?;
    let written = file
        .write(line.as_bytes())
        .map_err(|err| Error::io(path, err))?;
    if written != line.len() {
        return Err(Error::io(
            path,
            io::Error::new(io::ErrorKind::WriteZero, "short append to the hit log"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shard_is_exactly_two_lowercase_hex_characters() {
        let path = Path::new("shard");
        for good in ["00", "09", "ab", "ff"] {
            assert!(require_shard(good, path).is_ok(), "{good}");
        }
        for bad in ["", "0", "abc", "AB", "zz", "aG", "a0b"] {
            assert!(
                matches!(require_shard(bad, path), Err(Error::Corrupt { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_rest_is_exactly_thirty_eight_lowercase_hex_characters() {
        let path = Path::new("rest");
        let flat = "0".repeat(HEX_LEN - SHARD_LEN);
        assert!(sparse_id("ab", &flat, path).is_ok());

        let mut upper = flat.clone();
        upper.replace_range(37..38, "A");
        assert!(matches!(
            sparse_id("ab", &upper, path),
            Err(Error::Corrupt { .. })
        ));

        for length in [0, 1, HEX_LEN - SHARD_LEN - 1, HEX_LEN - SHARD_LEN + 1] {
            let short = "0".repeat(length);
            assert!(
                matches!(sparse_id("ab", &short, path), Err(Error::Corrupt { .. })),
                "{length}"
            );
        }

        assert!(matches!(
            sparse_id("ab", &"z".repeat(HEX_LEN - SHARD_LEN), path),
            Err(Error::Corrupt { .. })
        ));
    }
}
