//! A record book for checkpoint flags.
//!
//! A [`Store`] holds sessions, a session holds ctf flags, and a ctf flag holds
//! verification hits. Every id is a 40-character hex object name stored in the
//! sparse layout `<first two characters>/<rest>`, so the filesystem tree is the
//! index. Nothing is cached: each call reads the tree, and a hit appended by
//! another process is visible on the next read.
//!
//! ```
//! use ckpt::{Store, Verify};
//!
//! let dir = tempfile::tempdir().expect("temp dir");
//! let store = Store::at(dir.path());
//! let session = store.session_new("perf plots").expect("session");
//! let flag = store.flag_new(&session, "plot-read receipt").expect("flag");
//! store.verify(&flag, &Verify::new()).expect("hit");
//! assert_eq!(store.status(flag.as_id()).expect("status").flags[0].hits, 1);
//! ```

mod error;
mod id;
mod record;
mod store;

pub use error::Error;
pub use id::{FlagId, Id, SessionId};
pub use record::{FlagStatus, SessionSummary, Status, Target};
pub use store::{Store, Verify};
