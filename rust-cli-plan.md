# Lanes - Read-Only Context Gatherer (First Slice)

A briefing for a Claude Code session. This describes the first buildable piece of a
larger project. Read the whole thing before writing code. Where it says "verify on
the machine," do that first, because some on-disk formats drift between tool versions
and this plan must not be trusted over what the actual system shows.

## What Lanes is (just enough context)

Lanes is a context manager for a developer's working environment. The central concept
is a **lane**: a named view onto a single unit of work (for example one issue on one
git worktree of a project). A lane spans several tools at once - a terminal, a browser,
an editor, notes - and switching lanes will eventually re-establish that whole view.

This first slice does NOT switch anything. It only observes. The goal is a program that
walks the running environment and gathers the current context into a typed data
structure that can be serialized and eyeballed. Everything else in Lanes (capture,
restore, dashboards, lane inference) reads from this layer, so it is the correct
foundation to build first, not a throwaway prototype.

## Vocabulary (use these names consistently in code)

- **Facet**: a dimension of context. The closed set for now is Terminal, Browser,
  Editor, Notes. A facet is a category, not an instance.
- **Provider** (a.k.a. driver): the code that knows how to talk to one facet's backend
  (Zellij, Firefox via brotab, the Claude session store, etc.). This is a class/type.
- **Resource**: a live, ephemeral thing in the world (a Zellij session, a browser tab,
  an editor window). Discovered at read time, never owned by Lanes.
- **Selector**: a durable, provider-specific handle that records how to re-find a
  resource later. Opaque to the core. We record selectors now even though we will not
  act on them until a later slice.
- **Shape**: a provider-specific description of a resource's internal arrangement
  (a tab layout, a cursor position, a selected pane). Here it is read-only output:
  we capture the current shape, we do not impose one.
- **State**: what a resource is currently doing. Split into two questions: liveness
  (is it there at all) and activity (given that it is there, what is it doing, e.g.
  a Claude session waiting for input versus running).

Concepts deliberately OUT of scope for this slice: **Binding** (a lane-held pointer)
and **Lane** itself. We are not assigning anything to lanes yet. See Non-Goals.

## Core principles (these decide most design questions)

1. **Durable handle vs ephemeral locator.** A resource's stable identity (a Zellij
   session name, a Claude session UUID, a stamped profile/window name) is durable and
   gets recorded. The runtime numeric ids (Zellij pane ids, brotab tab ids, OS window
   ids) are ephemeral and get re-resolved on each read. Never persist an ephemeral id
   as identity.
2. **Reconcile on read.** There is no daemon and no central registry in the backends.
   Every read walks the world fresh and reports what is actually there right now.
3. **The snapshot is lane-agnostic.** At read time there are no lanes. Record only the
   raw associations you can actually observe (a session's cwd, a worktree path, a
   profile name, a window title). Organizing those into lanes is a separate, later
   problem that takes this snapshot as its input. Do not guess lanes here.
4. **The core stays ignorant of backend shape.** Deep, app-specific structure
   (window then tab then cursor) lives inside the relevant provider as opaque selector
   and shape data. The central types never unpack it.

## Language and stack

Use **Rust**. The heart of this project is type modeling, which is where Rust is
strongest, and this read-only slice exercises exactly that (enums + parsing into typed
data) while touching almost none of the write-side async orchestration that would be
the awkward part. Any pre-existing prototype code is disposable; do not try to preserve
it.

- `serde` + `serde_json` for the typed data model and snapshot output.
- Shelling out to external tools: `std::process::Command` is fine. Start synchronous;
  the reads are simple and sequential. Do not reach for async/tokio unless a real need
  appears.
- Model the closed provider set as **enums with `match` dispatch**. No `dyn` trait
  objects and no `async_trait`. Per-provider concrete types live in their own modules.

## Data model (read-only subset)

This is the trimmed version of the model. It keeps `enumerate`, `observe`, and a
read-only `capture` of shape, plus `Selector` for recording. It drops the entire write
side (no surface, no shape(want), no Binding, no activate).

```rust
use serde::{Serialize, Deserialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Facet { Terminal, Browser, Editor, Notes }

// Durable, provider-specific handle for re-finding a resource later.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "facet")]
pub enum Selector {
    Terminal(TerminalSel),   // e.g. zellij session name (+ optional pane name)
    Browser(BrowserSel),     // e.g. profile + stamped window mark + tab url
    Editor(EditorSel),       // project/worktree path + file (later slices)
    Notes(NotesSel),         // vault path (later slices)
}

// Read-only here: the observed current arrangement, not a desired one.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "facet")]
pub enum Shape {
    Browser(BrowserShape),   // observed tab layout / pinned set
    Terminal(TerminalShape), // observed selected pane/tab
    // No Editor/Notes shape yet; absent is just absent.
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status { Idle, Busy, NeedsAttention, Gone }

// Rich, provider-native state. The core never branches on this; it is recorded
// for display and later use. Only `Status` is the cross-facet projection.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProviderState {
    Claude(ClaudeState), // Running | AwaitingInput | ...
    Repl(ReplState),     // Idle | RunningCommand | ForegroundServer
    Tab(TabState),       // Loading | Complete
}

#[derive(Clone, Serialize, Deserialize)]
pub struct State { pub status: Status, pub detail: Option<ProviderState> }

// One observed thing in the world, fully described for the snapshot.
#[derive(Clone, Serialize, Deserialize)]
pub struct Observed {
    pub facet: Facet,
    pub selector: Selector,          // how we would re-find it (durable)
    pub locator: String,             // ephemeral runtime id; display/debug only
    pub label: Option<String>,       // human title if the backend exposes one
    pub shape: Option<Shape>,        // current arrangement, if observable
    pub state: Option<State>,        // current activity, if observable
    // Raw observable associations - the lane-agnostic clues. No interpretation.
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub extra: serde_json::Value,    // anything else a provider wants to stash
}

// The whole gathered picture at one instant.
#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub taken_at: String,            // RFC3339
    pub resources: Vec<Observed>,
}
```

Each provider implements two read operations, as plain module functions (no trait
needed yet): an `enumerate()` that returns the `Observed` list for its facet, and
whatever internal helpers it needs to fill in `state`/`shape`. The top level just
concatenates every provider's `enumerate()` into one `Snapshot` and serializes it.

## First-slice backends (in build order)

Pick the backends with clean, structured read surfaces. Defer the messy ones.

1. **Claude sessions (pure file reads, no extra software needed).** Start here.
2. **Zellij (structured CLI output).**
3. **Firefox tabs via brotab (needs the extension + native messaging installed).**
4. Correlation pass (descriptive only).

Explicitly deferred for now: macOS window enumeration (Hammerspoon/AX), Obsidian, and
any editor (IntelliJ/Cursor). They are real later facets but are the messiest reads and
are not needed to validate the model.

### Backend 1: Claude sessions

Reported layout (VERIFY on the machine first, it has drifted across versions):
- Transcripts live under `~/.claude/projects/<encoded-project-path>/<session-uuid>.jsonl`.
- The folder name is the project's absolute path slug-encoded (slashes to dashes),
  e.g. `/home/user/myapp` -> `-home-user-myapp`. Some installs hash it instead, and
  some nest under a `sessions/` subdirectory. Inspect the real tree before parsing.
- Filename stem is the session UUID. This is the durable handle - put it in the
  `TerminalSel`/selector and treat it as identity.
- Recency = file mtime.
- A `type: "summary"` record inside the JSONL carries the title/auto-name. Do not rely
  on the custom name being stable (auto-naming can overwrite it); record it as `label`
  but key everything on the UUID.
- Per-session sidecar data exists keyed by the same UUID, e.g. todos at
  `~/.claude/todos/{session-id}-*.json`. Optional to read now; useful later.

Activity (`State`): derive coarsely from the tail of the JSONL. If the last record is
an assistant turn with nothing after it (and/or a Stop hook fired), treat as Idle /
possibly NeedsAttention; if a user message follows the last assistant response, treat
as Busy. Map to `Status` and stash the precise reading in `ProviderState::Claude`.

Note: a Claude session normally lives inside a Zellij pane, so once Zellij is wired up,
correlate the Claude session's cwd with a Zellij session/pane rather than treating them
as unrelated.

### Backend 2: Zellij

- `zellij list-sessions` enumerates sessions. The session NAME is the durable handle.
- Use the action/listing commands to enumerate tabs and panes; prefer machine-readable
  output where the installed version offers it. VERIFY the exact subcommand names and
  output format against the installed Zellij version before parsing.
- Pane ids are typed (`terminal_N` / `plugin_N`, bare `N` == `terminal_N`) and are
  ephemeral runtime handles, so they go in `locator`, never in `selector`. Each terminal
  pane also exposes `$ZELLIJ_PANE_ID` to processes inside it.
- Capture `cwd` and the running command per pane where available; these are the main
  lane-agnostic clues for later correlation.

### Backend 3: Firefox tabs via brotab

- Requires the brotab CLI (`bt`) plus its browser extension and native-messaging host
  installed. VERIFY `bt clients` reports a connected Firefox before relying on it; if
  not present, the provider should degrade gracefully (return empty + a warning, not a
  crash).
- `bt list` returns lines of `prefix.window_id.tab_id <TAB> Title <TAB> URL`. Parse the
  window id, tab id, title, and url. `bt windows` and `bt clients` enumerate windows and
  connected browsers.
- window/tab ids are ephemeral -> `locator`. The durable-ish handle is the URL plus the
  profile/window mark; put those in `BrowserSel`. (Stamping stable window names is a
  later concern; for read-only just record what is there.)

### Backend 4: Correlation pass (descriptive only)

After the three providers populate the snapshot, add a non-destructive pass that notes
likely associations without committing to lanes: e.g. group resources that share a cwd
or a worktree path, or whose Firefox tab url references a repo/issue matching a
worktree. Output these as annotations on the snapshot, not as lane assignments. This is
the raw material the future lane layer will consume.

## Deliverable shape

The real artifact is a **library**, with a thin CLI over it. The gatherer is never run
for its own sake (nobody wants to read a JSON dump of their environment); its output
exists to feed the rest of Lanes (the future capture step, dashboard, lane switcher).
So structure it accordingly:

- Put the gathering logic in the crate's library (`lib.rs` + provider modules) and
  expose a single entry point, roughly `gather() -> Snapshot`. This is the durable
  thing; everything downstream becomes a caller of it.
- The binary (`main.rs`) is thin: it calls `gather()`, serializes, and prints. It is a
  diagnostic surface for eyeballing output and validating the model, not the product.
- The gather entry point must not stash state in globals or rely on process-lifetime
  setup. It should behave identically whether called once from `main` or repeatedly
  from a long-running process later. In practice: take inputs as arguments, return the
  `Snapshot`, own no hidden mutable state.

This costs almost nothing now and keeps the one-shot-to-resident-process path open
without building any of it. Do not tangle the logic into `main`; that only has to be
undone the moment the second consumer appears, which is soon.

## Output

Serialize `Snapshot` to pretty JSON. Default to stdout; allow `--out <path>` to write a
file. The immediate purpose is for a human to eyeball whether the `Observed` / `State` /
`Shape` shapes survive contact with reality. If they do not fit cleanly, that is the
cheapest possible moment to adjust the model, so treat a mismatch as a finding to report
back, not something to paper over.

## Non-goals (do not build these in this slice)

- No writing or mutation of any kind: no surfacing windows, no rearranging tabs, no
  selecting panes, no launching anything.
- No `Binding`, no `Lane`, no lane assignment. The snapshot is lane-agnostic.
- No window management / Hammerspoon, no macOS Accessibility queries.
- No Obsidian, no editor integration.
- No third-party/runtime-loaded providers. The provider set is closed and compiled in.
- No persistence layer or registry beyond writing the JSON snapshot out.
- No resident process / daemon. Shape the gather entry point so a daemon could call it
  later (see Deliverable shape), but build only the one-shot binary now.

## Suggested milestones

1. Project skeleton + the data-model types above (with per-provider leaf types stubbed),
   a `Snapshot` that serializes to JSON, and a `main` that prints an empty snapshot.
2. Claude-sessions provider populating `Observed` entries (UUID, label, cwd, recency,
   coarse activity). Eyeball the JSON.
3. Zellij provider added; begin correlating Claude sessions to panes by cwd.
4. brotab provider added, with graceful degradation when brotab is absent.
5. Correlation annotations pass.

At each milestone, dump the snapshot and sanity-check the shapes before moving on.

## Verify on the machine before coding

- The real layout under `~/.claude/` (project folder naming, whether a `sessions/`
  subdir exists, where the summary/title actually lives).
- The installed Zellij version and the exact session/tab/pane listing commands and their
  output format.
- Whether brotab is installed and `bt clients` shows a connected Firefox.

Do not trust this document over what these checks reveal.
