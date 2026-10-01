//! The `ckpt` command line.

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

use ckpt::{Error, FlagId, SessionId, Status, Store, Target, Verify};

/// Record book for checkpoint flags.
#[derive(Parser)]
#[command(name = "ckpt", version, about = "Record book for checkpoint flags")]
struct Cli {
    /// Store root; defaults to $CKPT_ROOT, then $TMPDIR/ckpt
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a session, or list the record book's sessions
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Add a ctf flag to a session
    Flag {
        #[command(subcommand)]
        command: FlagCommand,
    },
    /// Record a verification hit for a ctf flag and print the hit count read
    /// back from the log
    Verify {
        /// ctf flag id, flat or sparse
        flag: String,
        /// Note to store with the hit
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Print a session, looked up by session id or by any of its ctf flag ids
    Status {
        /// Session or ctf flag id, flat or sparse
        id: String,
        /// Emit the report as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Create a session and print its id
    New {
        /// What the session is for
        #[arg(long, value_name = "TEXT")]
        desc: String,
    },
    /// List every session in the record book
    List {
        /// Emit the report as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum FlagCommand {
    /// Add a ctf flag to a session and print its id
    New {
        /// Session id, flat or sparse
        session: String,
        /// What the flag stands for
        #[arg(long, value_name = "TEXT")]
        desc: String,
    },
}

/// The exit code when a command did its work but could not report it.
const REPORT_FAILED: u8 = 3;

/// Write one line to standard output, reporting a failed write rather than
/// panicking on it: a pipeline that closes early is not a crash.
fn report(line: &str) -> Result<(), Error> {
    use std::io::Write;

    writeln!(io::stdout().lock(), "{line}").map_err(|err| Error::io("stdout", err))
}

/// Report what a command did, or say on standard error that it was done and
/// could not be reported. The work is not repeated and not undone.
fn report_done(line: &str, what: &str) -> ExitCode {
    match report(line) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ckpt: {what}, but reporting it failed: {err}; {line}");
            ExitCode::from(REPORT_FAILED)
        }
    }
}

/// The code for a command whose report could not be written, whatever the
/// command had done by then.
fn report_failed(what: &str, err: &Error) -> ExitCode {
    eprintln!("ckpt: {what}, but reporting it failed: {err}");
    ExitCode::from(REPORT_FAILED)
}

fn main() -> ExitCode {
    let Cli { root, command } = Cli::parse();
    let store = root.map_or_else(Store::from_env, Store::at);
    match run(&store, command) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("ckpt: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(store: &Store, command: Command) -> Result<ExitCode, Error> {
    match command {
        Command::Session { command } => match command {
            SessionCommand::New { desc } => {
                let session = store.session_new(&desc)?;
                return Ok(report_done(
                    &session.to_string(),
                    &format!("session {session} was created"),
                ));
            }
            SessionCommand::List { json } => {
                let sessions = store.sessions()?;
                let printed = if json {
                    print_json(&sessions)
                } else if sessions.is_empty() {
                    report(&format!("no sessions in {}", store.root().display()))
                } else {
                    sessions.iter().try_for_each(|session| {
                        report(&format!(
                            "{}  flags {}  hits {}  {}",
                            session.session, session.flags, session.hits, session.desc
                        ))
                    })
                };
                if let Err(err) = printed {
                    return Ok(report_failed("the book was read", &err));
                }
            }
        },
        Command::Flag { command } => match command {
            FlagCommand::New { session, desc } => {
                let session = SessionId::parse(&session)?;
                let flag = store.flag_new(&session, &desc)?;
                return Ok(report_done(
                    &flag.to_string(),
                    &format!("flag {flag} was created"),
                ));
            }
        },
        Command::Verify { flag, note } => {
            let flag = FlagId::parse(&flag)?;
            let recorded = store.verify(&flag, &Verify { note, at: None })?;
            // The count the log holds now, read back from the log itself so a
            // hit which landed alongside this one is included.
            let read_back = store.flag_status(&flag).map(|status| status.hits);
            let (hits, code) = reported_count(recorded.hits, read_back);
            let line = format!("flag {} hits={hits}", recorded.id);
            // Whatever went wrong, the hit is in the log: the code says so even
            // when the count could not be written out.
            let reported = report_done(&line, "the hit was recorded");
            return Ok(if code != 0 {
                ExitCode::from(code)
            } else {
                reported
            });
        }
        Command::Status { id, json } => {
            // Either kind of id is written the same way; which kind it is, the
            // book decides.
            let id = SessionId::parse(&id)?;
            let status = store.status(id.as_id())?;
            let printed = if json {
                print_json(&status)
            } else {
                print_status(&status)
            };
            if let Err(err) = printed {
                return Ok(report_failed("the book was read", &err));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What `verify` prints and the code it exits with, from the count the append
/// recorded and the attempt to read the log back.
///
/// A failed read-back is reported on standard error and gets its own code: the
/// hit is recorded, so the command did its work, and a caller can tell that
/// apart from nothing having been recorded.
fn reported_count(recorded: u64, read_back: Result<u64, Error>) -> (u64, u8) {
    match read_back {
        Ok(hits) => (hits, 0),
        Err(err) => {
            eprintln!(
                "ckpt: the hit was recorded, but reading it back failed, so the count below is the one from before it: {err}"
            );
            (recorded, REPORT_FAILED)
        }
    }
}

fn print_status(status: &Status) -> Result<(), Error> {
    report(&format!(
        "session {}  created {}",
        status.session, status.created
    ))?;
    report(&format!("desc    {}", status.desc))?;
    report(&format!("flags   {}", status.flags.len()))?;
    for flag in &status.flags {
        let marker = match &status.matched {
            Target::Flag { id } if id == &flag.id => "*",
            _ => " ",
        };
        let last = flag
            .last_hit
            .map_or_else(|| "-".to_owned(), |ts| ts.to_string());
        report(&format!(
            "{marker} {}  hits {:>4}  last {}  {}",
            flag.id, flag.hits, last, flag.desc
        ))?;
    }
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<(), Error> {
    let text = serde_json::to_string_pretty(value).expect("record-book values serialize");
    report(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_back_failure_reports_the_recorded_count_and_its_own_code() {
        assert_eq!(
            reported_count(2, Ok(5)),
            (5, 0),
            "the count that was read back is the one printed"
        );

        let unreadable = reported_count(
            2,
            Err(Error::NotFound {
                kind: "flag",
                id: "x".to_owned(),
            }),
        );
        assert_eq!(unreadable.0, 2, "the recorded count stands");
        assert_eq!(unreadable.1, REPORT_FAILED);
        assert_eq!(REPORT_FAILED, 3, "the code is the one the README states");
    }
}
