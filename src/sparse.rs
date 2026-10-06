//! The sparse `<a>/<b>` object tree.
//!
//! An object name is laid out on disk as `<first two characters>/<remaining
//! 38>`, the fan-out git uses for object names. This module owns that layout and
//! the filesystem work around it: mapping a name to the path it occupies,
//! reading a directory the store owns, checking that a path is the real
//! directory or file it should be, and creating the directories the store owns
//! one level at a time.
//!
//! Everything here is about a path, never about what a record means: the store
//! decides what lives at a path, and calls these to make sure the path is what
//! it claims to be.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use storekit::RootedRelativePath;
use storekit::atomic::{self, PathKind, RootDir};

use crate::error::Error;
use crate::id::{HEX_LEN, Id};
use crate::substrate;

/// The number of leading characters of an object name that name a directory.
pub(crate) const SHARD_LEN: usize = 2;

/// The number of characters left for the entry inside that directory.
const REST_LEN: usize = HEX_LEN - SHARD_LEN;

/// The two path components of an object name.
pub(crate) fn components(id: &Id) -> (&str, &str) {
    id.as_str().split_at(SHARD_LEN)
}

/// The path an object name occupies, relative to the directory holding it.
pub(crate) fn path(id: &Id) -> PathBuf {
    let (shard, rest) = components(id);
    Path::new(shard).join(rest)
}

/// Parse an object name, flat or in sparse `ab/rest` form.
pub(crate) fn parse_id(input: &str) -> Result<Id, Error> {
    match input.split_once('/') {
        None => Id::parse(input),
        Some((shard, rest)) if shard.len() == SHARD_LEN && !rest.contains('/') => {
            Id::parse(&format!("{shard}{rest}"))
        }
        Some(_) => Err(Error::InvalidId {
            input: input.to_owned(),
        }),
    }
}

/// What an absent directory means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    /// Absent and empty are the same thing: a directory the store has not needed
    /// yet.
    Empty,
    /// Absent is a broken record: the directory belongs to something that is
    /// already there.
    Error,
}

/// The directory that holds `path`, with `path`'s own name as the validated
/// relative entry, or `Ok(None)` when that directory is not there at all.
///
/// The parent is canonicalized before it is opened so a root the caller chose
/// as a link stays allowed ([`RootDir::open`] refuses a symlinked root). Every
/// level below the root is checked one level at a time before a mutation
/// reaches it, so canonicalizing the immediate parent does not follow a link
/// the store itself planted: a link at any owned level is refused by the call
/// that would use that level.
///
/// `Ok(None)` means the parent is absent, so `path` is absent too; the caller
/// decides whether that is an absence or a failure.
fn open_parent(path: &Path) -> Result<Option<(RootDir, RootedRelativePath)>, Error> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Error::io(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a store path needs a directory and a name",
            ),
        ));
    };
    let canonical = match fs::canonicalize(parent) {
        Ok(canonical) => canonical,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::io(path, err)),
    };
    let root = RootDir::open(&canonical).map_err(|err| substrate::at(path, err))?;
    let rel = RootedRelativePath::parse(Path::new(name)).map_err(|err| substrate::at(path, err))?;
    Ok(Some((root, rel)))
}

/// The failure for a path whose directory is not there.
fn directory_missing(path: &Path) -> Error {
    Error::io(
        path,
        io::Error::new(io::ErrorKind::NotFound, "the store directory is not there"),
    )
}

/// The non-dot entries of a directory the store owns, sorted by name.
///
/// Dotfiles belong to the operating system and are skipped; every other entry
/// must be an object name. A path that is not a real directory is a foreign
/// entry, whether it is a file, a symlink to a directory, or a dangling
/// symlink. The directory itself is enumerated through
/// [`storekit::atomic::read_dir_fd`], which resolves each component relative to
/// the opened directory and classifies every entry without following it; the
/// shard layout and the meaning of an absence stay here.
pub(crate) fn entries(dir: &Path, missing: Missing) -> Result<Vec<(String, PathBuf)>, Error> {
    let Some((root, rel)) = open_parent(dir)? else {
        return match missing {
            Missing::Empty => Ok(Vec::new()),
            Missing::Error => Err(directory_missing(dir)),
        };
    };
    match atomic::path_kind_fd(&root, &rel).map_err(|err| substrate::at(dir, err))? {
        Some(PathKind::Dir) => {}
        Some(_) => {
            return Err(Error::Corrupt {
                path: dir.to_path_buf(),
                detail: "a store directory must be a real directory".to_owned(),
            });
        }
        None => {
            return match missing {
                Missing::Empty => Ok(Vec::new()),
                Missing::Error => Err(directory_missing(dir)),
            };
        }
    }

    let entries = atomic::read_dir_fd(&root, &rel).map_err(|err| substrate::at(dir, err))?;
    let mut owned = Vec::new();
    for entry in entries {
        let Some(name) = entry.name.to_str().map(str::to_owned) else {
            return Err(Error::Corrupt {
                path: dir.join(&entry.name),
                detail: "file name is not valid UTF-8".to_owned(),
            });
        };
        if name.starts_with('.') {
            continue;
        }
        owned.push((name, dir.join(&entry.name)));
    }
    owned.sort();
    Ok(owned)
}

/// Report a shard directory whose name is not exactly two lowercase-hex
/// characters.
///
/// A shard is checked before its contents, so a foreign shard is reported even
/// when it is empty and no object name is ever reassembled from it.
pub(crate) fn require_shard(shard: &str, path: &Path) -> Result<(), Error> {
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
pub(crate) fn sparse_id(shard: &str, rest: &str, path: &Path) -> Result<Id, Error> {
    let canonical = shard.len() == SHARD_LEN
        && rest.len() == REST_LEN
        && is_lowercase_hex(shard)
        && is_lowercase_hex(rest);
    if !canonical {
        return Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: format!(
                "`{shard}/{rest}` is not a {SHARD_LEN}/{REST_LEN}-character lowercase hex object name"
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

/// Whether the two directories directly above a record are real directories.
///
/// `Ok(false)` means one of them is absent, so the record it would hold is
/// absent too. A file or a symlink in the place of any of them is a foreign
/// entry: it is reported rather than followed, so a link planted above a record
/// cannot make a write land outside the store. A gap below does not hide a
/// foreign entry above it.
pub(crate) fn ancestors_are_real(dir: &Path) -> Result<bool, Error> {
    let mut complete = true;
    // Every ancestor is looked at, even after one is found missing: a foreign
    // entry higher up is reported rather than hidden behind a gap below it.
    for ancestor in dir.ancestors().skip(1).take(2) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Error::Corrupt {
                    path: ancestor.to_path_buf(),
                    detail: "a store directory must be a real directory".to_owned(),
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => complete = false,
            Err(err) => return Err(Error::io(ancestor, err)),
        }
    }
    Ok(complete)
}

/// Report a path that must be a real directory.
///
/// The final component is classified with [`storekit::atomic::path_kind_fd`],
/// which does not follow it: a symlink, a file, or any other foreign entry is
/// reported rather than used.
pub(crate) fn require_directory(path: &Path) -> Result<(), Error> {
    let Some((root, rel)) = open_parent(path)? else {
        return Err(directory_missing(path));
    };
    match atomic::path_kind_fd(&root, &rel).map_err(|err| substrate::at(path, err))? {
        Some(PathKind::Dir) => Ok(()),
        Some(_) => Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: "a store directory must be a real directory".to_owned(),
        }),
        None => Err(directory_missing(path)),
    }
}

/// Create a directory the store owns, or report a foreign entry in its place.
///
/// The final component is classified first, so an existing link or other
/// foreign entry is reported rather than used; a real directory that is already
/// there is tolerated. A missing entry is created with
/// [`storekit::atomic::create_dir_fd`], which does not follow a link at the
/// final component.
pub(crate) fn ensure_directory(path: &Path) -> Result<(), Error> {
    let Some((root, rel)) = open_parent(path)? else {
        return Err(directory_missing(path));
    };
    match atomic::path_kind_fd(&root, &rel).map_err(|err| substrate::at(path, err))? {
        Some(PathKind::Dir) => Ok(()),
        Some(_) => Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: "a store directory must be a real directory".to_owned(),
        }),
        None => match atomic::create_dir_fd(&root, &rel) {
            Ok(()) => Ok(()),
            // A racing creation: re-classify, so a directory that appeared is
            // tolerated and a foreign entry is still reported.
            Err(err) => match atomic::path_kind_fd(&root, &rel) {
                Ok(Some(PathKind::Dir)) => Ok(()),
                Ok(Some(_)) => Err(Error::Corrupt {
                    path: path.to_path_buf(),
                    detail: "a store directory must be a real directory".to_owned(),
                }),
                _ => Err(substrate::at(path, err)),
            },
        },
    }
}

/// Create a fresh record directory, which must not already exist.
///
/// A shard directory may be shared by many records, so it is created tolerantly;
/// a record directory belongs to one id, and reusing the name would overwrite
/// the record already there. Any entry already at the name is reported as the
/// name being taken, exactly as `create_dir`'s `AlreadyExists` was.
pub(crate) fn create_record_dir(path: &Path) -> Result<(), Error> {
    let Some((root, rel)) = open_parent(path)? else {
        return Err(directory_missing(path));
    };
    if atomic::path_kind_fd(&root, &rel)
        .map_err(|err| substrate::at(path, err))?
        .is_some()
    {
        return Err(Error::io(
            path,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an object name already holds a record",
            ),
        ));
    }
    atomic::create_dir_fd(&root, &rel).map_err(|err| substrate::at(path, err))
}

/// Report a file the store owns that is not a regular file of its own.
///
/// A symlink, or a file with more than one hard link, would let a read or an
/// append land somewhere else in the filesystem.
pub(crate) fn require_regular_file(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|err| Error::io(path, err))?;
    if !metadata.is_file() {
        return Err(Error::Corrupt {
            path: path.to_path_buf(),
            detail: "a store file must be a regular file".to_owned(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() > 1 {
            return Err(Error::Corrupt {
                path: path.to_path_buf(),
                detail: "a store file must not be linked to another file".to_owned(),
            });
        }
    }
    Ok(())
}

/// The bytes of a regular file the store owns, read through an open root.
///
/// `Ok(None)` means the file is not there. A symlink, a directory, a FIFO, or
/// a file with more than one hard link is refused with this crate's `Corrupt`,
/// naming `path`; the bytes come from [`storekit::atomic::read_fd`], which
/// resolves `rel` component-wise through the root descriptor, so a link
/// planted at any component is refused rather than followed.
pub(crate) fn read_regular_file(
    root: &RootDir,
    rel: &RootedRelativePath,
    path: &Path,
) -> Result<Option<Vec<u8>>, Error> {
    match atomic::path_kind_fd(root, rel).map_err(|err| substrate::at(path, err))? {
        None => Ok(None),
        Some(_) => {
            require_regular_file(path)?;
            atomic::read_fd(root, rel)
                .map(Some)
                .map_err(|err| substrate::at(path, err))
        }
    }
}

/// The text of a file the store owns, with the directories above it checked.
///
/// `Ok(None)` means the file is not there.
///
/// The entry's own directory is pinned as the read root: `ancestors_are_real`
/// has just checked it is a real directory, and [`RootDir::open`] refuses a
/// link in its place, so the read cannot follow one.
pub(crate) fn read_owned_file(path: &Path) -> Result<Option<String>, Error> {
    if !ancestors_are_real(path)? {
        return Ok(None);
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Error::io(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "a store file needs a directory and a name",
            ),
        ));
    };
    let root = RootDir::open(parent).map_err(|err| substrate::at(path, err))?;
    let rel = RootedRelativePath::parse(Path::new(name)).map_err(|err| substrate::at(path, err))?;
    let Some(bytes) = read_regular_file(&root, &rel, path)? else {
        return Ok(None);
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|err| Error::io(path, io::Error::new(io::ErrorKind::InvalidData, err)))
}

/// Write a file the store owns, which must not already be there.
///
/// A file is written once: a name that already holds one is reported rather than
/// overwritten, so a collision cannot destroy what landed there first.
pub(crate) fn write_new_file(path: &Path, contents: &str) -> Result<(), Error> {
    let Some((root, rel)) = open_parent(path)? else {
        return Err(directory_missing(path));
    };
    match atomic::path_kind_fd(&root, &rel).map_err(|err| substrate::at(path, err))? {
        // A file already holds the name. This pre-check is what makes the
        // write-once rule hold: `write_atomic_cas_fd` treats a byte-identical
        // rewrite as an idempotent success, which is NOT this crate's answer.
        // The substrate's compare-and-replace is reached only for an absent
        // name, and it still refuses a symlink at the final component.
        Some(PathKind::File) => {
            return Err(Error::io(
                path,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a file already holds this name",
                ),
            ));
        }
        Some(_) => {
            return Err(Error::Corrupt {
                path: path.to_path_buf(),
                detail: "a store file must be a regular file".to_owned(),
            });
        }
        None => {}
    }

    atomic::write_atomic_cas_fd(&root, &rel, contents.as_bytes())
        .map_err(|err| substrate::at(path, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_name_is_split_after_two_characters() {
        let id = Id::parse(&"ab".repeat(20)).expect("id");
        let (shard, rest) = components(&id);
        assert_eq!(shard, "ab");
        assert_eq!(rest.len(), REST_LEN);
        assert_eq!(path(&id), Path::new("ab").join(rest));
    }

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
        let flat = "0".repeat(REST_LEN);
        assert!(sparse_id("ab", &flat, path).is_ok());

        let mut upper = flat.clone();
        upper.replace_range(REST_LEN - 1..REST_LEN, "A");
        assert!(matches!(
            sparse_id("ab", &upper, path),
            Err(Error::Corrupt { .. })
        ));

        for length in [0, 1, REST_LEN - 1, REST_LEN + 1] {
            let short = "0".repeat(length);
            assert!(
                matches!(sparse_id("ab", &short, path), Err(Error::Corrupt { .. })),
                "{length}"
            );
        }

        assert!(matches!(
            sparse_id("ab", &"z".repeat(REST_LEN), path),
            Err(Error::Corrupt { .. })
        ));
    }

    #[test]
    fn a_record_directory_is_never_reused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ab").join("rest");
        fs::create_dir_all(path.parent().expect("parent")).expect("create");
        create_record_dir(&path).expect("the first record takes the name");

        assert!(
            matches!(create_record_dir(&path), Err(Error::Io { .. })),
            "a name that already holds a record is not reused"
        );
    }

    #[test]
    fn a_store_file_is_written_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("entry");
        write_new_file(&path, "first\n").expect("the first write takes the name");

        assert!(
            matches!(write_new_file(&path, "second\n"), Err(Error::Io { .. })),
            "a name that already holds a file is not overwritten"
        );
        assert_eq!(fs::read_to_string(&path).expect("read"), "first\n");
    }

    #[test]
    fn a_store_file_is_not_written_through_a_foreign_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("entry");
        fs::create_dir(&path).expect("put a directory where the file belongs");

        assert!(matches!(
            write_new_file(&path, "value\n"),
            Err(Error::Corrupt { .. })
        ));
    }

    #[test]
    fn a_missing_store_file_is_not_there() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("entry");

        assert!(matches!(read_owned_file(&path), Ok(None)));
    }
}
