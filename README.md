# ckpt

A record book for checkpoint flags.

A **session** is one unit of work. A session holds **ctf flags**: object names
planted in the material an agent is required to read. Each flag holds **hits**,
one per recorded verification, so a flag's hit count is the number of times its
verification was reported.

## Where the data lives

The store root is `$CKPT_ROOT`, falling back to `$TMPDIR/ckpt`. Nothing is
cleaned up: the tree sits in the temporary directory and the operating system
clears it on reboot.

Every id is a 40-character hex object name stored in the sparse layout git uses
for object names — the first two characters name a directory, the rest name the
entry:

```
<root>/sessions/<a>/<b>/
    meta.json                       {"desc": "...", "created": "2026-10-01T12:00:00Z"}
    flags/<a>/<b>/
        meta.json                   {"desc": "...", "created": "...", "counter": 1}
        hits.log                    one JSON line per verification
```

A session exists once its `meta.json` does, and a ctf flag exists once its own
`meta.json` does inside a session that exists. Dotfiles in these directories are left alone, so an operating
system droppings file does not disturb a read.

A ctf flag's `counter` is the flags the session showed when it was added, plus
one. A report lists a session's flags by `created`, then by `counter`, and then
by object name. Two additions that race each other can take the same counter, so
the order a report shows is the order the flags were added except where two of
them landed in the same instant.

`by-flag/` is the book's one mapping: which session holds a ctf flag. Its entry
is a session id and nothing else, so no record field is stored twice. It is
written before the flag's own files, so a flag that can be read is always
mapped, and every read checks the mapping against the tree: a flag the mapping
does not give to the session being reported, a mapping whose session does not
hold the flag, and a mapping that is not a session id are reported rather than
followed.

Looking a flag up reads its mapping entry and then the flags of the session the
mapping names, so it costs a file read and the owner's own flags instead of a
walk over every session. The mapping is read by key rather than enumerated, so a
name in `by-flag/` that nothing looks up is never read; an entry that is a
directory, a link, or not a session id is reported when it is looked up. Reads always come from disk; nothing is cached, so a
hit appended by another process is visible on the next read. A hit is written as
one complete line in a single append, so concurrent verifications both land.
`verify` prints the log's count when it reads back; if that read fails it says so
on standard error and prints the count the append recorded instead, and `status`
is the authority on the total.

## Commands

```
ckpt session new --desc <TEXT>         create a session, print its id
ckpt session list [--json]             list sessions
ckpt flag new <SESSION> --desc <TEXT>  add a ctf flag, print its id
ckpt verify <FLAG> [--note <TEXT>]     record a hit and print the count read back from the log
ckpt status <SESSION|FLAG> [--json]    print a session, by either id
```

`--root <DIR>` overrides the store root for one invocation.

`session new` and `flag new` print the bare id on stdout so it can be captured:

```sh
SESSION=$(ckpt session new --desc 'perf plots, iteration 7')
FLAG=$(ckpt flag new "$SESSION" --desc 'plot-read receipt')
```

## Programmatic API

The crate is a library as well as a binary:

```rust
use ckpt::{Store, Verify};

let store = Store::from_env();
let session = store.session_new("perf plots, iteration 7")?;
let flag = store.flag_new(&session, "plot-read receipt")?;
store.verify(&flag, &Verify::new())?;
let status = store.status(flag.as_id())?;
```

`Store::at(path)` roots a store at an explicit directory, which is what the
tests use. `status` takes either a session id or a flag id and reports which one
matched. `Verify` carries the hit's note and instant, so a caller can record a
verification with a chosen timestamp.

## How a flag is used

The description records what a flag stands for; the hit count records how many
times its verification was reported.

1. Add a flag to the session and read its id.
2. Plant that id in the material the agent must read, and keep no other copy.
3. The agent prints the id in its response.
4. Compare what the agent printed against the id that was planted, then record
   the outcome with `ckpt verify <FLAG>`.

Flags are written to the record book in plaintext, and the record book is
readable by anything that can read the temporary directory. The ledger records
the verification; the comparison in step 4 is the check.
