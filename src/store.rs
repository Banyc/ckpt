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
//! Every path the store would have written itself is checked for what it is:
//! a missing record is absent, a real directory with a regular `meta.json` is a
//! record, and anything else — a file, a symlink, a `meta.json` that is not a
//! regular file — is reported as a foreign entry rather than read as an absent
//! record.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::error::Error;
use crate::id::{FlagId, HEX_LEN, Id, SHARD_LEN, SessionId};
use crate::record::{FlagStatus, Hit, Meta, SessionSummary, Status, Target};

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
    pub fn session_new(&self, desc: &str) -> Result<SessionId, Error> {
        let session = SessionId::generate();
        let flags = self.flags_dir(&session);
        fs::create_dir_all(&flags).map_err(|err| Error::io(&flags, err))?;
        write_meta(&self.session_dir(&session), desc)?;
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
        // The `flags/` entry belongs to the session. A missing or foreign one
        // is reported rather than created here, so a write never heals a
        // damaged tree behind the read paths' back.
        require_directory(&self.flags_dir(session))?;

        let flag = FlagId::generate();
        let flag_dir = self.flag_dir(session, &flag);
        fs::create_dir_all(&flag_dir).map_err(|err| Error::io(&flag_dir, err))?;
        let hits = hits_path(&flag_dir);
        File::create(&hits).map_err(|err| Error::io(&hits, err))?;
        write_meta(&flag_dir, desc)?;
        Ok(flag)
    }

    /// Append one verification hit for `flag` and report its fresh count.
    ///
    /// An `Ok` result means exactly one hit was appended. An error means the
    /// record was rejected before anything was written, so a caller may retry
    /// without double-counting.
    pub fn verify(&self, flag: &FlagId, settings: &Verify) -> Result<FlagStatus, Error> {
        let session = self.flag_owner(flag)?.ok_or_else(|| Error::NotFound {
            kind: "flag",
            id: flag.to_string(),
        })?;
        let dir = self.flag_dir(&session, flag);

        // Read before writing: a damaged log is reported without a hit having
        // been appended.
        let meta = read_meta(&dir)?;
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
            let meta = read_meta(&self.session_dir(&session))?;
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

    fn session_dir(&self, session: &SessionId) -> PathBuf {
        self.sessions_dir().join(session.sparse_path())
    }

    fn flags_dir(&self, session: &SessionId) -> PathBuf {
        self.session_dir(session).join("flags")
    }

    fn flag_dir(&self, session: &SessionId, flag: &FlagId) -> PathBuf {
        self.flags_dir(session).join(flag.sparse_path())
    }

    fn session_ids(&self) -> Result<Vec<SessionId>, Error> {
        let mut ids = Vec::new();
        for (shard, shard_path) in owned_entries(&self.sessions_dir(), Missing::Empty)? {
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
        let meta = read_meta(&dir)?;
        let hits = read_hits(&hits_path(&dir))?;
        Ok(FlagStatus {
            id: flag.clone(),
            desc: meta.desc,
            created: meta.created,
            hits: hits.len() as u64,
            last_hit: hits.last().map(|hit| hit.ts),
        })
    }

    fn flag_statuses(&self, session: &SessionId) -> Result<Vec<FlagStatus>, Error> {
        self.flag_ids(session)?
            .iter()
            .map(|flag| self.flag_status(session, flag))
            .collect()
    }

    fn session_status(&self, session: &SessionId, matched: Target) -> Result<Status, Error> {
        let meta = read_meta(&self.session_dir(session))?;
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
/// A missing directory holds nothing, and a real directory with a regular
/// `meta.json` holds one. A file or symlink at the object name, or a
/// `meta.json` that is not a regular file, is a foreign entry: it is reported
/// rather than read as an absent record, so a damaged tree cannot answer "no
/// such session".
fn holds_record(dir: &Path) -> Result<bool, Error> {
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

fn read_meta(dir: &Path) -> Result<Meta, Error> {
    let path = meta_path(dir);
    let text = fs::read_to_string(&path).map_err(|err| Error::io(&path, err))?;
    serde_json::from_str(&text).map_err(|err| Error::Corrupt {
        path,
        detail: err.to_string(),
    })
}

fn write_meta(dir: &Path, desc: &str) -> Result<(), Error> {
    let path = meta_path(dir);
    let meta = Meta {
        desc: desc.to_owned(),
        created: Timestamp::now(),
    };
    let text = serde_json::to_string(&meta).map_err(|err| Error::Corrupt {
        path: path.clone(),
        detail: err.to_string(),
    })?;
    fs::write(&path, format!("{text}\n")).map_err(|err| Error::io(&path, err))
}

/// Read every hit line. A blank or unparseable line is a damaged record: the
/// writer only ever appends a complete JSON object, so anything else is
/// reported rather than counted or skipped.
fn read_hits(path: &Path) -> Result<Vec<Hit>, Error> {
    let text = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
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
