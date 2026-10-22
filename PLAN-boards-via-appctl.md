# Plan: switch Boards through its `appctl` socket instead of `open boards://`

For a session working in this repo, starting cold. Read `lanes-cli/README.md`
("Target drivers", "Spawning fire-and-forget children") and `DECISIONS.md`
first, as usual.

## Why

The `boards` target driver switches projects with
`open "boards://open?project=<project>"` (`activate_target` in
`lanes-cli/src/lib.rs`), and placement finds the window by bundle ID
`com.bmiller.boards` (`target_bundle_id`). Both only work for an
**installed** Boards:

- `boards://` is delivered by LaunchServices, only to a registered `.app`,
  and has misrouted between copies before (Boards commit `6993054`).
- A `just dev` Boards binary has **no bundle ID**. Verified: `lsappinfo`
  reports `CFBundleIdentifier=[ NULL ]` and
  `NSRunningApplication.bundleIdentifier` is nil. So `lanes-wm` can't find
  it by bundle ID.

So iterating on Boards with Lanes means `just install-dev` every time.
Boards now has a way around both problems.

## What exists now (all built and verified, 2026-10-20)

- **Boards** serves an `appctl` socket per running instance:
  `~/.local/state/bmiller/boards/instances/installed.sock` (the copy in
  `/Applications`) or `dev-<branch>.sock` (e.g. `dev-main` under
  `just dev`). It answers `open_project`, which switches the window and
  brings it forward through the same Rust code as `boards://` links
  (`boards/src-tauri/src/open_project.rs`). Verified on an installed build
  and a `just dev` build. See `~/src/projects/boards/README.md` "Instance
  socket".
- **`appctl::client`** (`~/src/dev-infra/crates/appctl/src/client.rs`,
  docs in `~/src/dev-infra/README.md`):
  ```rust
  let root = appctl::default_state_root();                 // Option<PathBuf>: APPCTL_STATE_DIR, else ~/.local/state/bmiller
  let mut conn = appctl::client::connect(&root, "boards", None)?;   // Result<Connection, ClientError>
  conn.hello().pid                                         // u32
  conn.request("open_project", args)                       // Result<Value, RequestError::{App(msg), Io(msg)}>
  ```
  - With no instance named, `connect` picks exactly one live `dev-*`, else
    `installed`; several live `dev-*` is `ClientError::Ambiguous`; nothing
    live is `ClientError::NoRunningInstance`.
  - It probes only that app's sockets. A leftover socket file refuses
    instantly; a stuck instance costs at most the 1s hello timeout. Requests
    time out after 5s.
- **`lanes-wm apply`** already accepts `"pid"` instead of `"app"` in a
  placement (`app_target_from` in `~/src/projects/lanes-wm/src/main.rs`,
  `AppTarget::Pid`, added for multiple Firefox profiles; see its README).
- **`appctl` CLI** (`~/.cargo/bin/appctl`): `appctl ls`,
  `appctl info boards`, `appctl send boards open_project project=X`. Handy
  for manual checks.

## Changes (all in `lanes-cli`)

### 1. Dependency

`lanes-cli/Cargo.toml`: `appctl = { path = "../../../dev-infra/crates/appctl" }`
(`~/src/projects/lanes/lanes-cli` -> `~/src/dev-infra`). `appctl` depends
only on `serde`/`serde_json`, which `lanes-cli` already has. `clippy.toml`'s
`msrv = "1.92.0"` is fine for it.

Switch UI (`ui/switch-ui`) depends on `lanes-cli` by path, so it picks up
`appctl` transitively. Its `just dev` `cargo watch` only watches
`lanes-cli`, so an `appctl` change needs a manual restart there (see
`~/src/projects/CONVENTIONS.md`, "Hot-reloading a Rust path dependency").

### 2. Activation can report what to place

`activate_target` returns `Result<(), String>` today and placement always
uses `target_bundle_id`. Let activation optionally say *which process* it
reached, e.g. return `Result<Option<u32>, String>` (a pid), and in
`apply_targets`' placement loop use `"pid": <pid>` when one came back,
else `"app": <bundle id>` as today. Only the Boards arm returns a pid for
now. Keep the existing `minimized` and `launch` paths on the bundle ID
(unchanged behaviour; `launch` means "start the installed app").

### 3. Boards driver: socket first, `open` as fallback

```
match appctl::client::connect(root, "boards", None):
  Ok(conn)  -> conn.request("open_project", {"project": project})
                 Ok(_)                 -> Ok(Some(conn.hello().pid))
                 Err(App(msg))         -> Err(msg)        // e.g. "no such project: X"; no fallback, open would fail too
                 Err(Io(msg))          -> warn, fall back to open
  Err(NoRunningInstance)               -> fall back to open (quietly: this is the normal "Boards isn't running" case,
                                          and `open boards://` cold-launches the installed app, as today)
  Err(Ambiguous / Unresponsive / ...)  -> warn (message names the instances), fall back to open
fallback: open "boards://open?project=..."   -> Ok(None)   // placement uses the bundle ID, as today
```

- Pass the state root in (from `appctl::default_state_root()`) so tests can
  point it at a temp dir. If it's `None`, go straight to the fallback.
- Warnings go through the existing `warnings` vector and `eprintln!`, like
  every other target failure in `apply_targets`.
- Log timing with the existing `logging::perf` pattern, e.g.
  `focus.boards_appctl` with the outcome, so a slow socket shows up in
  `perf.log`.
- The call is synchronous and short, and runs on `apply_targets`' per-target
  thread like every activation. It spawns no child process, so
  `spawn_detached` doesn't apply.
- `open_project` already focuses Boards' window, same as the `open` link
  did, so `raise` semantics don't change.

### 4. Docs

- `lanes-cli/README.md` "Target drivers": the `boards` row becomes
  "`appctl` socket `open_project` on the running instance (dev or
  installed), placed by pid; falls back to `open 'boards://...'`".
- Add a short paragraph on instance choice (one live `dev-*`, else
  `installed`) and that a `just dev` Boards now works with Lanes without
  `just install-dev`.
- If the change is a meaningful design decision for this repo, record it
  in `DECISIONS.md` in that file's style.

## Open question for the user

**Which instance Lanes drives** is the `appctl` default: one live `dev-*`,
else `installed`. If you'd rather lanes always prefer `installed`, or
choose per lane (e.g. a `board.toml` `instance = "dev-main"` field), say so
before building. The plan assumes the default; a per-lane field can be
added later without changing the rest (`connect`'s third argument).

## Tests

`cargo test` with zero warnings (lanes' usual bar; check `lanes-cli`'s own
conventions first).

- Boards driver against a real `appctl` server in a temp state dir:
  `appctl::start(appctl::Config::new("boards", <temp git repo>, "0").state_dir(<short /tmp dir>).command("open_project", ...))`.
  The `appctl` tests (`~/src/dev-infra/crates/appctl/tests/client.rs`) show
  the helpers: short `/tmp` paths (macOS caps socket paths at ~104 bytes),
  `git init -b main` plus an empty commit for the temp repo.
  - live instance: command received with the right project, pid returned
  - app error (`no such project`): returned as `Err`, no fallback
  - no instance: fallback chosen (make the `open` call injectable or
    split decision from execution, so tests don't run `open`)
  - two live `dev-*`: warning plus fallback
- Placement JSON: a pid from activation produces `"pid"`, otherwise
  `"app"` as before (existing `target_bundle_id` tests stay).

## Manual checks

Lanes with a Boards target already exist: `boards-dev` and `dev-infra`
(`~/.config/lanes/contexts/<id>/board.toml`). Switch with
`lanes focus <lane>`.

1. Installed Boards running: `lanes focus boards-dev` switches Boards to
   that project and places the window; check `perf.log` for the
   `focus.boards_appctl` line.
2. `just dev` in `~/src/projects/boards` (it quits the installed copy): the
   same switch now drives and places the **dev** window, with no install.
   This is the goal.
3. Boards not running at all: the switch falls back to `open boards://`,
   which launches the installed app on that project, as today.
4. A lane whose `board.toml` names a non-existent project: a warning with
   `no such project`, and no fallback.

## Not in this plan

- Using Boards' pushed state (e.g. current project) for Lanes signals:
  needs `appctl` subscriptions (`~/src/dev-infra/PLAN-appctl-events.md`).
- Other drivers over `appctl`: only Boards serves it so far.
- A lensd `appctl` source (`~/src/dev-infra/NOTES-tauri-app-source.md`).
