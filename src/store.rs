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
//! write rather than a record, and is treated as absent, so a partial addition
//! cannot answer as a session. A write that fails part way can also leave a
//! record that does not read, which every report reports as damaged: the record
//! book has no cleanup command, and its tree is a temporary directory.
//!
//! `<root>/by-flag/<a>/<b>` holds the one mapping the book keeps: which session
//! a ctf flag belongs to. It is a mapping and nothing else — one session id —
//! so no record field is stored twice in it. Its entry is written before the
//! flag's own files, so a flag that can be read is always mapped, and every read
//! checks the mapping against the tree: a flag the mapping does not give to the
//! session being reported, or a session the mapping names that does not hold the
//! flag, is reported rather than followed.
//!
//! A record may carry fields this version does not know; they are read past, so
//! a record written by a newer build stays readable here.
//!
//! A record directory is read by name — its `meta.json` and `hits.log` — so
//! other names inside it are ignored. The checks above are made before a path is
//! used rather than while it is open: they refuse links and foreign entries left
//! in the tree, they do not defend against another process swapping one in
//! mid-operation, because the store belongs to the process reading it.

use std::fs::{self, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::Error;
use crate::id::{FlagId, Id, SessionId};
use crate::record::{FlagMeta, FlagStatus, Hit, SessionMeta, SessionSummary, Status, Target};
use crate::sparse::{self, Missing};

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
        sparse::ensure_directory(&self.sessions_dir())?;
        sparse::ensure_directory(&self.session_shard_dir(&session))?;
        let dir = self.session_dir(&session);
        sparse::create_record_dir(&dir)?;
        sparse::ensure_directory(&self.flags_dir(&session))?;
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
        // And its id must not be mapped as a flag, which a listing reports too.
        self.require_not_both(&FlagId::from_id(session.as_id().clone()))?;
        // The `flags/` entry belongs to the session. A missing or foreign one
        // is reported rather than created here, so a write never heals a
        // damaged tree behind the read paths' back.
        sparse::require_directory(&self.flags_dir(session))?;
        // The session's flags are walked before anything is created, so a
        // rejected addition leaves no half-written record behind: the walk is
        // what validates every shard and the mapping.
        let counter = self.flag_ids(session)?.len() as u64 + 1;

        let flag = FlagId::generate();
        // The mapping is written first, so a refusal there leaves nothing of the
        // flag behind: what can be refused is refused before anything is made.
        self.write_owner(session, &flag)?;
        if let Err(err) = self.write_flag_record(session, &flag, desc, counter) {
            // The entry is this call's own, created under a name that must not
            // exist, so taking it back cannot take a record that belongs to
            // someone else. What remains is a directory with no record in it,
            // which every read treats as absent.
            return Err(self.take_back_mapping(&flag, err));
        }
        Ok(flag)
    }

    /// Write the record of a ctf flag whose mapping is already in place.
    fn write_flag_record(
        &self,
        session: &SessionId,
        flag: &FlagId,
        desc: &str,
        counter: u64,
    ) -> Result<(), Error> {
        let flag_dir = self.flag_dir(session, flag);
        sparse::ensure_directory(&self.flag_shard_dir(session, flag))?;
        sparse::create_record_dir(&flag_dir)?;
        let hits = hits_path(&flag_dir);
        sparse::write_new_file(&hits, "")?;
        write_record(
            &meta_path(&flag_dir),
            &FlagMeta {
                desc: desc.to_owned(),
                created: Timestamp::now(),
                counter,
            },
        )
    }

    /// Remove the mapping entry an addition left when the rest of it failed.
    ///
    /// The failure is the one that is reported; a mapping entry that could not be
    /// removed is reported with it, because that entry is what makes an id name
    /// a flag.
    fn take_back_mapping(&self, flag: &FlagId, err: Error) -> Error {
        let path = self.owner_path(flag);
        match fs::remove_file(&path) {
            Ok(()) => err,
            // The entry is not there, which is what this was for.
            Err(undo) if undo.kind() == io::ErrorKind::NotFound => err,
            Err(undo) => Error::io(
                &path,
                io::Error::new(
                    undo.kind(),
                    format!("{err}; and the mapping entry could not be removed: {undo}"),
                ),
            ),
        }
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
    ///
    /// The mapping decides whether an id is a ctf flag: an id that is mapped as
    /// a flag and also names a session is reported rather than resolved to one
    /// of them, so a flag id always leads to the session that holds it. A flag
    /// the mapping does not name is not a flag to a lookup, and is reported by
    /// the walk that lists the session holding it.
    pub fn status(&self, id: &Id) -> Result<Status, Error> {
        let session = SessionId::from_id(id.clone());
        let flag = FlagId::from_id(id.clone());
        let as_session = holds_record(&self.session_dir(&session))?;
        // A lookup reports an id that is both, so this never resolves one.
        let owner = self.flag_owner(&flag)?;
        if as_session {
            return self.session_status(&session, Target::Session);
        }
        match owner {
            Some(owner) => self.session_status(&owner, Target::Flag { id: flag }),
            None => Err(Error::NotFound {
                kind: "session or flag",
                id: id.to_string(),
            }),
        }
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

    fn by_flag_dir(&self) -> PathBuf {
        self.root.join("by-flag")
    }

    fn owner_shard_dir(&self, flag: &FlagId) -> PathBuf {
        self.by_flag_dir().join(sparse::components(flag.as_id()).0)
    }

    fn owner_path(&self, flag: &FlagId) -> PathBuf {
        self.by_flag_dir().join(flag.sparse_path())
    }

    fn session_shard_dir(&self, session: &SessionId) -> PathBuf {
        self.sessions_dir()
            .join(sparse::components(session.as_id()).0)
    }

    fn session_dir(&self, session: &SessionId) -> PathBuf {
        self.sessions_dir().join(session.sparse_path())
    }

    fn flags_dir(&self, session: &SessionId) -> PathBuf {
        self.session_dir(session).join("flags")
    }

    fn flag_shard_dir(&self, session: &SessionId, flag: &FlagId) -> PathBuf {
        self.flags_dir(session)
            .join(sparse::components(flag.as_id()).0)
    }

    fn flag_dir(&self, session: &SessionId, flag: &FlagId) -> PathBuf {
        self.flags_dir(session).join(flag.sparse_path())
    }

    fn session_ids(&self) -> Result<Vec<SessionId>, Error> {
        let mut ids = Vec::new();
        for (shard, shard_path) in sparse::entries(&self.sessions_dir(), Missing::Empty)? {
            sparse::require_shard(&shard, &shard_path)?;
            for (rest, rest_path) in sparse::entries(&shard_path, Missing::Empty)? {
                let session = SessionId::from_id(sparse::sparse_id(&shard, &rest, &rest_path)?);
                if !holds_record(&rest_path)? {
                    continue;
                }
                // An id that also names a ctf flag is reported rather than listed
                // as a session as well.
                self.require_not_both(&FlagId::from_id(session.as_id().clone()))?;
                ids.push(session);
            }
        }
        Ok(ids)
    }

    fn flag_ids(&self, session: &SessionId) -> Result<Vec<FlagId>, Error> {
        let mut ids = Vec::new();
        for (shard, shard_path) in sparse::entries(&self.flags_dir(session), Missing::Error)? {
            sparse::require_shard(&shard, &shard_path)?;
            for (rest, rest_path) in sparse::entries(&shard_path, Missing::Error)? {
                let flag = FlagId::from_id(sparse::sparse_id(&shard, &rest, &rest_path)?);
                if !holds_record(&rest_path)? {
                    continue;
                }
                // The mapping is the owner record: a flag it does not give to
                // this session is reported rather than counted here as well.
                if self.read_owner(&flag)?.as_ref() != Some(session) {
                    return Err(Error::Corrupt {
                        path: rest_path,
                        detail: format!("`{flag}` is not mapped to the session that holds it"),
                    });
                }
                // An id that also names a session is reported wherever flags are
                // listed, not only where that id is looked up.
                let as_session = SessionId::from_id(flag.as_id().clone());
                if holds_record(&self.session_dir(&as_session))? {
                    return Err(Error::Corrupt {
                        path: rest_path,
                        detail: format!("`{flag}` is both a session and a ctf flag"),
                    });
                }
                ids.push(flag);
            }
        }
        Ok(ids)
    }

    /// Report an id that is mapped as a ctf flag and also names a session.
    ///
    /// A session's own id is not mapped to a flag, so both a listing of sessions
    /// and an addition to one apply this before going further.
    fn require_not_both(&self, flag: &FlagId) -> Result<(), Error> {
        if self.read_owner(flag)?.is_some() {
            return Err(Error::Corrupt {
                path: self.owner_path(flag),
                detail: format!("`{flag}` is both a session and a ctf flag"),
            });
        }
        Ok(())
    }

    /// The session the mapping gives this ctf flag to.
    fn read_owner(&self, flag: &FlagId) -> Result<Option<SessionId>, Error> {
        let path = self.owner_path(flag);
        let Some(text) = sparse::read_owned_file(&path)? else {
            return Ok(None);
        };
        // The writer ends the entry with a newline and writes nothing else, so
        // nothing else is accepted: the mapping holds one id, not a field to
        // search through.
        let id = text.strip_suffix('\n').unwrap_or(&text);
        SessionId::parse(id).map(Some).map_err(|_| Error::Corrupt {
            path,
            detail: "an index entry must be a session id and nothing else".to_owned(),
        })
    }

    /// Map a ctf flag to the session that holds it.
    ///
    /// The entry is written before the flag's own files, so a flag that can be
    /// read is always mapped.
    fn write_owner(&self, session: &SessionId, flag: &FlagId) -> Result<(), Error> {
        sparse::ensure_directory(&self.by_flag_dir())?;
        sparse::ensure_directory(&self.owner_shard_dir(flag))?;
        sparse::write_new_file(&self.owner_path(flag), &format!("{session}\n"))
    }

    /// The one session a ctf flag is recorded under.
    ///
    /// The mapping says who holds it, and that statement is checked against the
    /// owner's own record: the session must read, and its flags are walked the
    /// way a report walks them, so a lookup never calls a damaged tree fine. An
    /// entry whose session does not hold the flag is the book saying a flag is
    /// there when it is not, and is reported; an id with no entry at all is not
    /// a flag, and is not found.
    fn flag_owner(&self, flag: &FlagId) -> Result<Option<SessionId>, Error> {
        let Some(session) = self.read_owner(flag)? else {
            return Ok(None);
        };
        // The owner must be a record that reads, so a hit is not written into a
        // session every report calls corrupt. Listing its flags is also what
        // reports an id that names both a session and a ctf flag.
        if !holds_record(&self.session_dir(&session))? {
            return Err(Error::Corrupt {
                path: self.owner_path(flag),
                detail: format!("`{flag}` is mapped to a session that is not a record"),
            });
        }
        read_session_meta(&self.session_dir(&session))?;
        // Its flags are read the way a report reads them, and the mapping has to
        // name one of them: a mapping without its flag is reported, not passed
        // over as an absent flag.
        if !self
            .flag_statuses(&session)?
            .iter()
            .any(|status| &status.id == flag)
        {
            return Err(Error::Corrupt {
                path: self.owner_path(flag),
                detail: format!("`{flag}` is mapped to a session that does not hold it"),
            });
        }
        Ok(Some(session))
    }

    /// One ctf flag's status, looked up by the flag's own id.
    pub fn flag_status(&self, flag: &FlagId) -> Result<FlagStatus, Error> {
        let session = self.flag_owner(flag)?.ok_or_else(|| Error::NotFound {
            kind: "flag",
            id: flag.to_string(),
        })?;
        self.read_flag_status(&session, flag)
    }

    fn read_flag_status(&self, session: &SessionId, flag: &FlagId) -> Result<FlagStatus, Error> {
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

    /// A session's flags in the order they were added: by timestamp, then by the
    /// counter.
    ///
    /// A flag the mapping does not give to this session is reported by the walk
    /// in [`Store::flag_ids`], so a flag held under two sessions is never
    /// counted under both.
    fn flag_statuses(&self, session: &SessionId) -> Result<Vec<FlagStatus>, Error> {
        let mut statuses = Vec::new();
        for flag in self.flag_ids(session)? {
            statuses.push(self.read_flag_status(session, &flag)?);
        }
        statuses.sort_by(compare_status);
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
    if !sparse::ancestors_are_real(dir)? {
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
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::io(&meta, err)),
        Ok(metadata) if !metadata.is_file() => {
            return Err(Error::Corrupt {
                path: meta,
                detail: "meta.json must be a regular file".to_owned(),
            });
        }
        Ok(_) => {}
    }

    // A shard holds only records: a name or an entry beside this one that is not
    // one is reported, so a record is never read past a foreign sibling.
    if let Some(shard) = dir.parent() {
        let name = shard
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        for (rest, entry) in sparse::entries(shard, sparse::Missing::Empty)? {
            sparse::require_directory(&entry)?;
            sparse::sparse_id(&name, &rest, &entry)?;
        }
    }
    Ok(true)
}

/// A report's order: by instant, then by counter.
///
/// Two flags that agree on both keep the order they were found in, which is
/// their object names, because the sort is stable.
fn compare_status(left: &FlagStatus, right: &FlagStatus) -> std::cmp::Ordering {
    (left.created, left.counter).cmp(&(right.created, right.counter))
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
    sparse::require_regular_file(path)?;
    let text = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    serde_json::from_str(&text).map_err(|err| Error::Corrupt {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })
}

fn write_record<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let text = serde_json::to_string(value).map_err(|err| Error::Corrupt {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })?;
    sparse::write_new_file(path, &format!("{text}\n"))
}

/// The hits in a log.
///
/// A blank or unparseable line is a damaged record: the writer only ever appends
/// a complete JSON object followed by a newline, so anything else is reported
/// rather than counted or skipped.
fn read_hits(path: &Path) -> Result<Vec<Hit>, Error> {
    sparse::require_regular_file(path)?;
    let text = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = text.strip_suffix('\n') else {
        return Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: "the final line has no terminating newline".to_owned(),
        });
    };
    // The lines are split here rather than by `str::lines`, which hides a
    // carriage return: the writer appends one JSON object and a newline, and
    // anything else on the line is damage rather than something to parse past.
    let mut hits = Vec::new();
    for line in body.split('\n') {
        if line.trim() != line || line.contains('\r') {
            return Err(Error::Corrupt {
                path: path.to_path_buf(),
                detail: "a hit line must be one JSON object and nothing else".to_owned(),
            });
        }
        hits.push(serde_json::from_str(line).map_err(|err| Error::Corrupt {
            path: path.to_path_buf(),
            detail: err.to_string(),
        })?);
    }
    Ok(hits)
}

/// Append one hit as a single complete line.
///
/// The whole line is built in one buffer and handed to one `write` call, and
/// `O_APPEND` makes the offset update and that write atomic, so two concurrent
/// verifications cannot interleave halves of a line.
fn append_hit(path: &Path, hit: &Hit) -> Result<(), Error> {
    sparse::require_regular_file(path)?;
    let mut line = serde_json::to_string(hit).map_err(|err| Error::Corrupt {
        path: path.to_path_buf(),
        detail: err.to_string(),
    })?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|err| Error::io(path, err))?;
    // One write of the whole line; a write the filesystem cuts short is reported
    // and whatever it left in the log stays there to be read as damage.
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

    fn status_at(created: i64, counter: u64, id: &str) -> FlagStatus {
        FlagStatus {
            id: FlagId::parse(id).expect("id"),
            desc: String::new(),
            created: Timestamp::from_second(created).expect("instant"),
            counter,
            hits: 0,
            last_hit: None,
        }
    }

    #[test]
    fn a_report_orders_by_instant_then_counter_then_name() {
        use std::cmp::Ordering;

        let early = status_at(1_000, 2, &"1".repeat(40));
        let late = status_at(2_000, 1, &"2".repeat(40));
        assert_eq!(
            compare_status(&early, &late),
            Ordering::Less,
            "the instant decides first"
        );

        let first = status_at(1_000, 1, &"3".repeat(40));
        let second = status_at(1_000, 2, &"0".repeat(40));
        assert_eq!(
            compare_status(&first, &second),
            Ordering::Less,
            "the counter breaks a tie"
        );

        let lower = status_at(1_000, 1, &"0".repeat(40));
        let higher = status_at(1_000, 1, &"1".repeat(40));
        assert_eq!(
            compare_status(&lower, &higher),
            Ordering::Equal,
            "nothing beyond the instant and the counter is a key"
        );
    }

    #[test]
    fn a_record_file_is_never_written_through_a_foreign_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("meta.json");
        fs::create_dir(&path).expect("put a directory where meta.json belongs");

        assert!(matches!(
            write_record(&path, &1u64),
            Err(Error::Corrupt { .. })
        ));
    }

    #[test]
    fn a_record_write_failure_is_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("missing").join("meta.json");

        assert!(matches!(write_record(&path, &1u64), Err(Error::Io { .. })));
    }
}
