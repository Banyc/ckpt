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
use storekit::atomic::{self, RootDir};

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

/// The non-dot entries of a directory the store owns, sorted by name.
///
/// Dotfiles belong to the operating system and are skipped; every other entry
/// must be an object name. A path that is not a real directory is a foreign
/// entry, whether it is a file, a symlink to a directory, or a dangling
/// symlink.
pub(crate) fn entries(dir: &Path, missing: Missing) -> Result<Vec<(String, PathBuf)>, Error> {
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
pub(crate) fn require_directory(path: &Path) -> Result<(), Error> {
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
pub(crate) fn ensure_directory(path: &Path) -> Result<(), Error> {
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
pub(crate) fn create_record_dir(path: &Path) -> Result<(), Error> {
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
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(Error::Corrupt {
                path: path.to_path_buf(),
                detail: "a store file must be a regular file".to_owned(),
            });
        }
        Ok(_) => {
            return Err(Error::io(
                path,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a file already holds this name",
                ),
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(Error::io(path, err)),
    }

    fs::write(path, contents).map_err(|err| Error::io(path, err))
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
