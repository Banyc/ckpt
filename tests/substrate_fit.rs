//! The storekit surface this crate actually uses, cited by NAME.
//!
//! Deleting any item below is a COMPILE failure here, which is the point: a
//! migration onto a substrate crate is a coupling, and this file is where the
//! coupling is stated rather than implied. Items are cited by NAME, never by
//! `file:line`, because a dependency's line numbers move under it.
//!
//! # What this crate adopts
//!
//! * [`storekit::atomic::RootDir`] and [`storekit::RootedRelativePath`]
//!   — the owned root and the validated root-relative spelling every mutation
//!   takes. The root is CANONICALIZED before `RootDir::open`, because that
//!   function refuses a symlinked root while this crate permits a root that is
//!   "a link the caller chose".
//! * [`storekit::atomic::path_kind_fd`] — the live kind of a name, through the
//!   root descriptor. This is what lets a MISSING entry stay an absence
//!   (`Ok(None)`) while a file, symlink or foreign entry in its place is
//!   refused: this crate decides absence itself rather than reading an error
//!   out of a failed open.
//! * [`storekit::atomic::read_fd`] — the read, refusing a symlink in the final
//!   or any parent component.
//! * [`storekit::atomic::write_atomic_cas_fd`] — the "written once" create: a
//!   durable temp, fsync, `linkat`, and a parent fsync, refusing a symlink
//!   final entry.
//! * [`storekit::atomic::create_dir_fd`] — the shard and record directories.
//! * [`storekit::atomic::read_dir_fd`] — the listing whose names this crate
//!   splits into its own two-character shard layout.
//! * [`storekit::atomic::remove_file_fd`] — taking back a `by-flag` mapping.
//! * [`storekit::Error`] — translated at one boundary (`src/substrate.rs`) into
//!   this crate's own error type, which is what its tests pin.
//!
//! # What this crate deliberately does NOT adopt, and why
//!
//! These are reasons, not claims about the code; each is a property of this
//! crate's design that a substrate capability would change.
//!
//! * **No lock.** This record book is read and written by several processes at
//!   once, and its contract is that a hit appended by one process is visible to
//!   the next read of another. A lock would serialise that away; it would also
//!   put a non-dot record in the root that this crate's own shard walk reports
//!   as a foreign entry.
//! * **No `AppendTail`.** `storekit`'s append policy is a whole-file
//!   compare-and-replace with a stated lost-update hazard; this crate's
//!   `hits.log` is a true `O_APPEND` whose whole point is that concurrent
//!   verifications both land.
//! * **No `sync`, `manifest`, `transport`, or tree-copy primitive.** There is
//!   no tree to move and no manifest to keep: the record book's own files ARE
//!   its index, and nothing here is cached.
//! * **No `valid_name` / reserved-spelling machinery.** This crate's object
//!   names are 40 lowercase hex characters, which is strictly narrower than the
//!   substrate's id rule.

/// The adopted items, cited so their removal breaks this build.
#[test]
fn the_adopted_substrate_surface_still_exists() {
    // Functions: taking each as a value pins its existence AND its signature.
    let _read: fn(
        &storekit::atomic::RootDir,
        &storekit::RootedRelativePath,
    ) -> storekit::Result<Vec<u8>> = storekit::atomic::read_fd;
    let _kind: fn(
        &storekit::atomic::RootDir,
        &storekit::RootedRelativePath,
    ) -> storekit::Result<Option<storekit::atomic::PathKind>> = storekit::atomic::path_kind_fd;
    let _create_dir: fn(
        &storekit::atomic::RootDir,
        &storekit::RootedRelativePath,
    ) -> storekit::Result<()> = storekit::atomic::create_dir_fd;
    let _read_dir = storekit::atomic::read_dir_fd;
    let _remove: fn(
        &storekit::atomic::RootDir,
        &storekit::RootedRelativePath,
    ) -> storekit::Result<()> = storekit::atomic::remove_file_fd;
    let _cas = storekit::atomic::write_atomic_cas_fd;

    // Types and constructors.
    let _open: fn(&std::path::Path) -> storekit::Result<storekit::atomic::RootDir> =
        storekit::atomic::RootDir::open;
    let _parse: fn(&std::path::Path) -> storekit::Result<storekit::RootedRelativePath> =
        storekit::RootedRelativePath::parse;

    // The error type the boundary translates is named here so a rename breaks this build.
    let _error: fn(storekit::Error) -> String = |e| e.to_string();
}
