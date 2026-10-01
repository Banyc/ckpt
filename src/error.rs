//! The single error type every operation returns.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Everything that can go wrong when reading or writing the record book.
#[derive(Debug)]
pub enum Error {
    /// The input is not a 40-character hex object name.
    InvalidId { input: String },
    /// No object of this kind carries this id.
    NotFound { kind: &'static str, id: String },
    /// A file could not be read, created, or written.
    Io { path: PathBuf, source: io::Error },
    /// A file exists but does not hold the record it should.
    Corrupt { path: PathBuf, detail: String },
}

impl Error {
    pub(crate) fn io(path: impl AsRef<Path>, source: io::Error) -> Error {
        Error::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidId { input } => {
                write!(f, "`{input}` is not a 40-character hex object name")
            }
            Error::NotFound { kind, id } => write!(f, "no {kind} `{id}` in this record book"),
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Corrupt { path, detail } => write!(f, "{}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
