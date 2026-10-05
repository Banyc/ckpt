//! The boundary between `storekit`'s substrate errors and this crate's.
//!
//! This crate keeps its OWN error type: its variants are part of what its
//! tests pin (`Io`, `Corrupt`, `InvalidId`, `NotFound`), and a record book's
//! reader branches on those, not on a substrate's kinds. So a substrate failure
//! is TRANSLATED here, at the call site that knows which path was being
//! touched, rather than leaking a second error vocabulary into this crate's
//! public surface.
//!
//! The mapping is EXHAUSTIVE on purpose. `storekit::Error` is not
//! `#[non_exhaustive]`, so a variant added there stops this crate compiling —
//! which is the point: a new substrate failure class must be classified here,
//! not silently collapsed into `Io`.

use std::io;
use std::path::Path;

use crate::error::Error;

/// Translate a `storekit` failure that happened on `path`.
///
/// Data that exists but does not mean what it should becomes
/// [`Error::Corrupt`] with the substrate's own text as the detail; anything
/// mechanical becomes [`Error::Io`], keeping a real `io::Error` when the
/// substrate had one.
pub(crate) fn at(path: impl AsRef<Path>, e: storekit::Error) -> Error {
    let path = path.as_ref().to_path_buf();
    match e {
        // Mechanical, and the substrate preserved the OS error.
        storekit::Error::Io(source) => Error::Io { path, source },
        // A record that does not parse is damage in the book, not a failure
        // to read it.
        storekit::Error::Json(source) => Error::Corrupt {
            path,
            detail: format!("the record is not valid JSON: {source}"),
        },
        // A refusal from the substrate's path or materialization rules: the
        // entry is not something this crate may read or write through.
        storekit::Error::Path(detail) => Error::Corrupt { path, detail },
        storekit::Error::Materialization { message, .. } => Error::Corrupt {
            path,
            detail: message,
        },
        storekit::Error::Integrity(detail) => Error::Corrupt { path, detail },
        // The remaining classes are mechanical failures of the store: keep the
        // substrate's text, and keep a real `io::Error` underneath so a caller
        // that inspects the source still can. `NotFound` lands here too — this
        // crate decides absence BEFORE calling the substrate (a missing file is
        // `Ok(None)` here, not an error), so one arriving means the entry
        // vanished mid-operation, which is a mechanical failure.
        other @ (storekit::Error::Store { .. }
        | storekit::Error::Transport { .. }
        | storekit::Error::Preflight { .. }
        | storekit::Error::NotFound(_)
        | storekit::Error::Ref(_)
        | storekit::Error::Conflict(_)
        | storekit::Error::Reserved { .. }
        | storekit::Error::LockContended(_)) => Error::Io {
            path,
            source: io::Error::other(other.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Every reachable `storekit::Error` variant maps to this crate's `Io` or
    /// `Corrupt`, with a NON-EMPTY path and a non-empty detail. A variant that
    /// mapped to an empty path would print as ": something" and give a reader
    /// nothing to look at, which is why this is asserted rather than assumed.
    #[test]
    fn every_substrate_error_becomes_a_usable_io_or_corrupt() {
        let each: Vec<storekit::Error> = vec![
            storekit::Error::Io(io::Error::other("io")),
            storekit::Error::Json(
                serde_json::from_str::<serde_json::Value>("{").expect_err("a parse failure"),
            ),
            storekit::Error::Path("path".to_string()),
            storekit::Error::materialization("materialization"),
            storekit::Error::integrity("integrity"),
            storekit::Error::store("store"),
            storekit::Error::transport("transport"),
            storekit::Error::preflight("preflight"),
            storekit::Error::NotFound("not found".to_string()),
            storekit::Error::Ref("ref".to_string()),
            storekit::Error::Conflict("conflict".to_string()),
            storekit::Error::Reserved {
                reason: storekit::ReservedKind::ResidueBelow,
                message: "reserved".to_string(),
            },
            storekit::Error::LockContended("contended".to_string()),
        ];
        let path = PathBuf::from("/tmp/ckpt-substrate-test");
        for e in each {
            let mapped = at(&path, e);
            match &mapped {
                Error::Io { path, source } => {
                    assert_eq!(path, &path_of(&mapped));
                    assert!(!source.to_string().is_empty(), "{mapped}");
                }
                Error::Corrupt { path, detail } => {
                    assert_eq!(path, &path_of(&mapped));
                    assert!(!detail.is_empty(), "{mapped}");
                }
                other => panic!("a substrate error must map to Io or Corrupt, got {other:?}"),
            }
            assert!(!path_of(&mapped).as_os_str().is_empty(), "{mapped}");
        }
    }

    fn path_of(e: &Error) -> PathBuf {
        match e {
            Error::Io { path, .. } | Error::Corrupt { path, .. } => path.clone(),
            other => panic!("not an Io/Corrupt: {other:?}"),
        }
    }
}
