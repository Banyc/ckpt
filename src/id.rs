//! Sparse object-store identifiers.
//!
//! An [`Id`] is 20 random bytes rendered as 40 lowercase hex characters, the
//! shape git gives object names. On disk it occupies the sparse layout
//! `<first two characters>/<rest>`: a directory holds at most a few hundred
//! entries, and the filesystem tree is the index.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Serialize;

use crate::Error;

/// The number of hex characters in an object name.
pub const HEX_LEN: usize = 40;

/// The number of leading characters that name the first sparse path component.
pub const SHARD_LEN: usize = 2;

const ID_BYTES: usize = HEX_LEN / 2;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// A validated object name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(String);

impl Id {
    /// Draw a fresh id from the operating system.
    ///
    /// # Panics
    ///
    /// Panics if the operating system cannot supply randomness, in which case
    /// no id in this process could be distinct from any other.
    pub fn generate() -> Id {
        let mut bytes = [0u8; ID_BYTES];
        getrandom::fill(&mut bytes).expect("operating system randomness is available");
        Id(hex(&bytes))
    }

    /// Parse a 40-character hex object name, flat or in sparse `ab/rest` form.
    pub fn parse(input: &str) -> Result<Id, Error> {
        let flat = match input.split_once('/') {
            None => input.to_owned(),
            Some((shard, rest)) if shard.len() == SHARD_LEN && !rest.contains('/') => {
                format!("{shard}{rest}")
            }
            Some(_) => {
                return Err(Error::InvalidId {
                    input: input.to_owned(),
                });
            }
        };
        if flat.len() != HEX_LEN || !flat.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::InvalidId {
                input: input.to_owned(),
            });
        }
        Ok(Id(flat.to_ascii_lowercase()))
    }

    /// The flat 40-character form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The two path components of the sparse layout.
    pub fn sparse(&self) -> (&str, &str) {
        self.0.split_at(SHARD_LEN)
    }

    /// The sparse layout as a relative path.
    pub fn sparse_path(&self) -> PathBuf {
        let (shard, rest) = self.sparse();
        Path::new(shard).join(rest)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Id {
    type Err = Error;

    fn from_str(input: &str) -> Result<Id, Error> {
        Id::parse(input)
    }
}

impl Serialize for Id {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Declare a validated id for one kind of object.
macro_rules! object_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Id);

        impl $name {
            /// Draw a fresh id from the operating system.
            pub fn generate() -> $name {
                $name(Id::generate())
            }

            /// Parse a 40-character hex object name, flat or in sparse form.
            pub fn parse(input: &str) -> Result<$name, Error> {
                Ok($name(Id::parse(input)?))
            }

            /// Wrap an already-validated object name.
            pub fn from_id(id: Id) -> $name {
                $name(id)
            }

            /// The underlying object name.
            pub fn as_id(&self) -> &Id {
                &self.0
            }

            /// The sparse layout as a relative path.
            pub fn sparse_path(&self) -> PathBuf {
                self.0.sparse_path()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(input: &str) -> Result<$name, Error> {
                $name::parse(input)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.0.as_str())
            }
        }
    };
}

object_id! {
    /// A session: one unit of work in the record book.
    SessionId
}

object_id! {
    /// A ctf flag: an object name planted in material an agent must read.
    FlagId
}
