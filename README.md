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
        meta.json
        hits.log                    one JSON line per verification
```

A session exists once its `meta.json` does; a ctf flag exists once its own
`meta.json` does. Dotfiles in these directories are left alone, so an operating
system droppings file does not disturb a read.

Because a flag is stored inside its session, looking a flag up walks the
`flags/` entry of every session. Reads always come from disk; nothing is cached,
so a hit appended by another process is visible on the next read. A hit is one
appended line, so concurrent verifications both land.

## Commands

```
ckpt session new --desc <TEXT>         create a session, print its id
ckpt session list [--json]             list sessions
ckpt flag new <SESSION> --desc <TEXT>  add a ctf flag, print its id
ckpt verify <FLAG> [--note <TEXT>]     record a hit, print the new count
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
