<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow, LogicalSize, LogicalPosition, primaryMonitor } from "@tauri-apps/api/window";
  import { onMount, onDestroy, tick } from "svelte";

  // The panel is a fixed-width column of rows normally - width only needs
  // recomputing for edit mode, which adds a toggle switch to each row.
  const PANEL_WIDTH = 360;
  const EDIT_MODE_EXTRA_WIDTH = 36;

  let snapshot = null;
  let activeSignal = null;
  let dashboardEl;

  // get_snapshot() -> gather_lanes() is a real subprocess round-trip per
  // running Zellij session (~500-600ms typically - see the README's
  // Diagnostics section). refresh() is triggered independently by a 10s
  // timer AND every "sessions-changed" event, which in a normal working
  // session (active-sessions/*.json touched by hook writes, git index
  // changes, growing transcripts) can fire every 1-3s - faster than a
  // single refresh finishes. Without this guard, a new refresh would start
  // while the previous one's gather_lanes() was still mid-flight, and the
  // two would contend over the same Zellij sessions' IPC, dragging both out
  // past 1s (confirmed via perf.log's ui.refresh.done elapsed_ms spiking to
  // 1000-1700ms even with no switch involved). At most one refresh runs at
  // a time now; a trigger that arrives mid-refresh is coalesced into a
  // single follow-up rather than dropped, so nothing is missed.
  let refreshInFlight = false;
  let refreshPending = false;

  async function refresh() {
    if (refreshInFlight) {
      refreshPending = true;
      return;
    }
    refreshInFlight = true;
    const t0 = performance.now();
    invoke("log_ui_event", { event: "ui.refresh.start", detail: "" });
    try {
      snapshot = await invoke("get_snapshot");
      await tick();
      // Never resize/reposition the window while the overlay is showing -
      // clicking a signal opens the overlay, then immediately calls
      // refresh(), which used to resize the window right underneath it.
      // Programmatic win.setSize()/setPosition() on macOS can produce a
      // spurious momentary focus loss, and the focus-guard (see onMount)
      // treats any click landing during that window as "just focus the
      // window," swallowing it rather than letting it reach a button's
      // on:click - which is exactly what made the dismiss button
      // intermittently do nothing right after opening an overlay. Skipped
      // here regardless of whether that's the full mechanism, since
      // resizing out from under an open modal is bad UX either way; the
      // deferred call in dismissOverlay()/copyErrorReport's flow catches
      // up once the overlay actually closes.
      if (!activeSignal) await resizeToContent();
    } finally {
      invoke("log_ui_event", { event: "ui.refresh.done", detail: `elapsed_ms=${(performance.now() - t0).toFixed(1)}` });
      refreshInFlight = false;
      if (refreshPending) {
        refreshPending = false;
        refresh();
      }
    }
  }

  async function resizeToContent() {
    if (!dashboardEl) return;
    const width = panelWidth;
    const height = dashboardEl.offsetHeight;

    const win = getCurrentWindow();
    await win.setSize(new LogicalSize(width, height));

    const monitor = await primaryMonitor();
    if (monitor) {
      const scale = monitor.scaleFactor;
      const workX = monitor.workArea.position.x / scale;
      const workY = monitor.workArea.position.y / scale;
      const workW = monitor.workArea.size.width / scale;
      const workH = monitor.workArea.size.height / scale;
      const x = workX + (workW - width) / 2;
      const y = workY + (workH - height) / 2;
      await win.setPosition(new LogicalPosition(x, y));
    }
  }

  let timer;
  let unlistenSessions;
  let unlistenLane;
  let unlistenFocus;
  let unlistenEditMode;
  let unlistenShowInactive;
  let unlistenShowInactiveNoop;

  // state.kdl's edit-mode/show-inactive fields are the source of truth (see
  // src-tauri) - these mirror them locally rather than owning them, so the
  // tray checkbox, hypo+E/I, and this window's own Escape key all stay in
  // sync regardless of which one triggered the change.
  let editMode = false;
  let showInactive = false;
  $: panelWidth = PANEL_WIDTH + (editMode ? EDIT_MODE_EXTRA_WIDTH : 0);
  // get_snapshot() always returns every lane now - filtering which ones
  // actually render is done here, against whatever the last fetch already
  // has in memory, specifically so flipping editMode/showInactive is a pure
  // local re-render (resizeToContent measures the result) rather than
  // needing a whole new gather_lanes() round trip (~500-600ms of real
  // subprocess I/O - see the README's Diagnostics section) just to show or
  // hide rows that were sitting right here the whole time.
  $: visibleLanes = snapshot ? snapshot.lanes.filter(l => l.active || showInactive || editMode) : [];

  async function applyEditMode(enabled) {
    if (enabled === editMode) return;
    invoke("log_ui_event", { event: "ui.edit_mode_apply.start", detail: `enabled=${enabled}` });
    editMode = enabled;
    await tick();
    await resizeToContent();
    invoke("log_ui_event", { event: "ui.edit_mode_apply.resized", detail: `enabled=${enabled}` });
  }

  // Can't actually happen while editMode is on - the CLI's
  // ToggleShowInactive refuses to change the value at all in that case (see
  // pulseAcknowledge below for what fires instead), and the tray checkbox
  // is disabled too (see apply_edit_mode) - so this never needs to guard
  // against or acknowledge a same-but-not-rendered case.
  async function applyShowInactive(show) {
    if (show === showInactive) return;
    showInactive = show;
    await tick();
    await resizeToContent();
  }

  // Border pulse acknowledging a show-inactive keypress that had no effect
  // (edit mode was on - see ToggleShowInactive, which never sends
  // show-inactive-changed in that case, only this). Restarted by toggling
  // the class off then back on across a tick, since a CSS animation won't
  // replay on a class that's already applied.
  let pulseNoop = false;
  let pulseNoopTimer;

  function pulseAcknowledge() {
    pulseNoop = false;
    tick().then(() => {
      pulseNoop = true;
      clearTimeout(pulseNoopTimer);
      pulseNoopTimer = setTimeout(() => { pulseNoop = false; }, 300);
    });
  }

  function setEditMode(enabled) {
    invoke("set_edit_mode", { enabled });
  }

  // Whether the OS window was focused *before* the click currently in
  // flight. Read at mousedown (capture phase, so it runs before anything
  // else sees the event) rather than at click time, since a click on an
  // unfocused window focuses it as a side effect - by click time this would
  // already read true and the guard below would never trigger.
  let windowFocused = true;
  let suppressNextClick = false;

  onMount(async () => {
    refresh();
    timer = setInterval(refresh, 10000);
    editMode = await invoke("get_edit_mode");
    showInactive = await invoke("get_show_inactive");
    disabledClaudeSessions = new Set(await invoke("get_disabled_claude_sessions"));
    unlistenEditMode = await listen("edit-mode-changed", (event) => applyEditMode(event.payload));
    unlistenShowInactive = await listen("show-inactive-changed", (event) => applyShowInactive(event.payload));
    unlistenShowInactiveNoop = await listen("show-inactive-noop", () => pulseAcknowledge());
    unlistenSessions = await listen("sessions-changed", () => refresh());
    // Fires immediately on a lane/session change (see lib.rs) - update both
    // highlights right away instead of waiting on the slower full refresh
    // above, same as the optimistic local update already done for in-UI
    // signal clicks.
    unlistenLane = await listen("lane-changed", (event) => {
      if (snapshot) snapshot = { ...snapshot, focused_lane: event.payload.lane, focused_claude_session: event.payload.session };
      invoke("log_ui_event", {
        event: "ui.rendered_lane_changed",
        detail: `lane=${event.payload.lane ?? ""} session=${event.payload.session ?? ""}`,
      });
    });

    const win = getCurrentWindow();
    windowFocused = await win.isFocused();
    unlistenFocus = await win.onFocusChanged(({ payload }) => {
      windowFocused = payload;
    });

    // First click while unfocused should only focus the window - not also
    // fire whatever lane/signal happened to be under the cursor. Both
    // listeners run in the capture phase so they see the event before
    // Svelte's bubble-phase on:click handlers do.
    document.addEventListener("mousedown", (e) => {
      if (!windowFocused) {
        suppressNextClick = true;
        win.setFocus();
        return;
      }
      if (!e.target.closest(".signal") && !e.target.closest(".overlay")) {
        win.startDragging();
      }
    }, true);
    document.addEventListener("click", (e) => {
      if (suppressNextClick) {
        suppressNextClick = false;
        e.stopPropagation();
        e.preventDefault();
      }
    }, true);
  });
  onDestroy(() => {
    clearInterval(timer);
    if (unlistenSessions) unlistenSessions();
    if (unlistenLane) unlistenLane();
    if (unlistenFocus) unlistenFocus();
    if (unlistenEditMode) unlistenEditMode();
    if (unlistenShowInactive) unlistenShowInactive();
    if (unlistenShowInactiveNoop) unlistenShowInactiveNoop();
  });

  function allSignals(lane) {
    return lane.facets.flatMap(f => f.signals ?? []);
  }

  // Kind (which domain a signal is about) and reason (what's true within
  // that domain) are two separate fields on the backend Signal now - kind
  // drives the pill, reason drives the rest of the label. Neither is
  // guessed from the other.
  function kindLabel(signal) {
    if (signal.kind === "claude_session") return "claude";
    if (signal.kind === "repo") return "git";
    if (signal.kind === "lanes") return "lanes";
    return signal.kind;
  }

  function reasonLabel(signal) {
    if (signal.reason === "pending_commit") return "pending commit";
    if (signal.reason === "non_default_branch") return "non-default branch";
    if (signal.reason === "active") return "running";
    if (signal.reason === "awaiting") return "idle";
    if (signal.reason === "ready") return "ready";
    if (signal.reason === "permission") return "permission";
    if (signal.reason === "session_missing") return "session missing";
    if (signal.reason === "session_not_running") return "no zellij session";
    return signal.reason;
  }

  function signalLabel(signal) {
    return `${kindLabel(signal)} · ${reasonLabel(signal)}`;
  }

  // Both reasons mean the same thing: this lane's terminal isn't actually
  // there - session_missing (no cached WezTerm tab) and session_not_running
  // (no Zellij session at all) are the two Lanes-kind facts gather_lanes()
  // ever produces for that. Shared by the lane-click guard below and the
  // is-unreachable class on both the lane card and the chip itself, so
  // neither invites a hover affordance for something that's just an error
  // display, not a normal action.
  function isUnreachable(signal) {
    return signal.kind === "lanes" && (signal.reason === "session_missing" || signal.reason === "session_not_running");
  }

  function unreachableSignal(lane) {
    return allSignals(lane).find(isUnreachable);
  }

  async function handleLaneClick(lane) {
    const unreachable = unreachableSignal(lane);
    if (unreachable) {
      // Don't bother actually attempting the focus - it would just fail
      // via activate_wezterm_tab's own error, which is roundabout and
      // depends on wezterm's specific wording. We already know why it'd
      // fail: unreachable.detail already explains it (see lib.rs's
      // gather_lanes) - the overlay renders it the same way it would for
      // clicking the chip directly, no status/local explanation needed.
      activeSignal = { lane, signal: unreachable, status: null };
      return;
    }
    // Clicking the lane card itself (not a specific chip) routes through
    // whichever signal is first in display order and actually has
    // something to do - purely positional (leftmost/topmost first, same
    // order the chips render in), with no regard for kind or cyclable.
    // cyclable answers a different question entirely (would the *global*
    // hypo+J/K rotation land here, which requires the whole lane to be
    // active+reachable) - clicking a specific lane directly doesn't care
    // about that at all, it only cares what's actually sitting in it.
    const firstActionable = allSignals(lane).find(s => s.action);
    if (firstActionable) {
      await handleSignalClick(lane, firstActionable);
      return;
    }
    // Nothing actionable to route through (e.g. every signal here is
    // detail-only, or there are no signals at all) - fall back to plain
    // focus.
    snapshot = { ...snapshot, focused_lane: lane.id };
    await invoke("focus_lane", { laneId: lane.id });
    await refresh();
  }

  async function handleSignalClick(lane, signal) {
    const sessionId = signal.action?.kind === "switch_claude_session" ? signal.action.session_id : snapshot.focused_claude_session;
    snapshot = { ...snapshot, focused_lane: lane.id, focused_claude_session: sessionId };
    await invoke("set_focused_lane", { laneId: lane.id });
    if (signal.action) {
      const err = await invoke("execute_action", { action: signal.action }).then(() => null).catch(e => String(e));
      if (err) activeSignal = { lane, signal, status: err };
    } else {
      activeSignal = { lane, signal, status: null };
    }
    await refresh();
  }

  // Flips lane.active optimistically (so the switch itself and
  // visibleLanes' filter react immediately) and persists via the same
  // set_lane_active the CLI's `lanes activate`/`deactivate` already used -
  // edit mode is just a GUI for that existing per-lane flag, not a new one.
  async function toggleLaneActive(lane) {
    const active = !lane.active;
    snapshot = { ...snapshot, lanes: snapshot.lanes.map(l => l.id === lane.id ? { ...l, active } : l) };
    await invoke("set_lane_active", { laneId: lane.id, active });
  }

  // Excludes/re-includes one specific Claude session from sessions next/prev
  // cycling, keyed by its own session_id (see state::is_claude_session_
  // disabled on the backend for why that's a fine key despite the UUID
  // changing on restart) - crossed against a plain Set fetched once at
  // mount rather than a field threaded through every Signal, since it's
  // only ever meaningful for claude_session-kind chips.
  let disabledClaudeSessions = new Set();

  async function toggleSessionCycling(signal) {
    const sessionId = signal.action?.session_id;
    if (!sessionId) return;
    const disabled = !disabledClaudeSessions.has(sessionId);
    const next = new Set(disabledClaudeSessions);
    if (disabled) next.add(sessionId); else next.delete(sessionId);
    disabledClaudeSessions = next;
    await invoke("set_claude_session_disabled", { sessionId, disabled });
  }

  function dismissOverlay() {
    activeSignal = null;
    // Catch up on any resize that was skipped in refresh() while this
    // overlay was open (see the comment there) - window content may have
    // changed lane count/signal count during that time.
    resizeToContent();
  }

  async function copyErrorReport() {
    const s = activeSignal;
    if (!s) return;
    const report = [
      `lane: ${s.lane.name}`,
      `signal: ${signalLabel(s.signal)}`,
      `detail: ${s.signal.detail ?? "(none)"}`,
      `action: ${s.signal.action ? JSON.stringify(s.signal.action) : "none"}`,
      `error: ${s.status ?? "(none)"}`,
    ].join("\n");
    try {
      await navigator.clipboard.writeText(report);
    } catch (e) {
      console.error("copy failed", e);
    }
  }

  function handleKeydown(e) {
    if (e.key !== "Escape") return;
    if (activeSignal) { dismissOverlay(); return; }
    if (editMode) { setEditMode(false); return; }
  }
</script>

<svelte:window on:keydown={handleKeydown} />

{#if snapshot}
  <div class="dashboard" bind:this={dashboardEl} style="width: {panelWidth}px">
    <div class="panel" class:pulse-noop={pulseNoop}>
      {#each visibleLanes as lane}
        {@const signals = allSignals(lane)}
        <div
          class="lane"
          class:is-cyclable={lane.cyclable}
          class:is-focused={snapshot.focused_lane === lane.id}
          class:is-unreachable={!!unreachableSignal(lane)}
          on:click={() => handleLaneClick(lane)}
        >
          <div class="lane-body">
            <div class="lane-head"><span class="lane-name">{lane.name}</span></div>
            {#if signals.length > 0}
              <div class="signals" class:is-editing={editMode}>
                {#each signals as signal}
                  <span class="signal-wrap">
                    <button
                      class="signal urgency-{signal.urgency}"
                      class:is-active={signal.action?.kind === "switch_claude_session" && signal.action.session_id === snapshot.focused_claude_session}
                      class:is-cyclable={signal.cyclable}
                      class:is-unreachable={isUnreachable(signal)}
                      on:mousedown|stopPropagation={() => handleSignalClick(lane, signal)}
                      on:click|stopPropagation
                    ><span class="kind">{kindLabel(signal)}</span>{reasonLabel(signal)}</button>
                    <!-- Only for an active lane - an inactive one is already
                         excluded from cycling entirely (the lane-level
                         toggle above), so a per-session cycling toggle here
                         would be controlling something that's already off. -->
                    {#if editMode && lane.active && signal.kind === "claude_session"}
                      <button
                        class="session-toggle"
                        class:is-on={!disabledClaudeSessions.has(signal.action.session_id)}
                        aria-label={disabledClaudeSessions.has(signal.action.session_id) ? "Include in cycling" : "Exclude from cycling"}
                        on:click|stopPropagation={() => toggleSessionCycling(signal)}
                      ></button>
                    {/if}
                  </span>
                {/each}
              </div>
            {:else}
              <div class="lane-empty">no signals</div>
            {/if}
          </div>
          {#if editMode}
            <button
              class="lane-toggle"
              class:is-on={lane.active}
              aria-label={lane.active ? `Deactivate ${lane.name}` : `Activate ${lane.name}`}
              on:click|stopPropagation={() => toggleLaneActive(lane)}
            ></button>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{/if}

{#if activeSignal}
  <div class="backdrop" on:mousedown={dismissOverlay} role="presentation">
    <div class="overlay" on:mousedown|stopPropagation role="dialog">
      <div class="overlay-lane">{activeSignal.lane.name}</div>
      <div class="overlay-reason">{signalLabel(activeSignal.signal)}</div>
      {#if activeSignal.signal.detail}
        <div class="overlay-detail">{activeSignal.signal.detail}</div>
      {/if}
      {#if activeSignal.signal.action}
        <div class="overlay-action">{JSON.stringify(activeSignal.signal.action)}</div>
      {:else}
        <div class="overlay-action">no action</div>
      {/if}
      {#if activeSignal.status}
        <div class="overlay-status">{activeSignal.status}</div>
      {/if}
      <div class="overlay-buttons">
        {#if activeSignal.status || activeSignal.signal.detail}
          <button class="overlay-copy" title="copy report" on:click={copyErrorReport}>⧉</button>
        {/if}
        <button class="overlay-dismiss" on:click={dismissOverlay}>dismiss</button>
      </div>
    </div>
  </div>
{/if}

<style>
  :global(*, *::before, *::after) { box-sizing: border-box; margin: 0; padding: 0; user-select: none; }

  /* Status semantics: idle-Claude and pending-commit aren't warnings -
     they're "something finished, ready for you," a GOOD state, not a
     caution one. So the middle tier is green (ready), not amber. Only
     genuine blocking (a permission prompt Claude can't get past without
     you) earns red. Ambient/running gets a calm, receding slate-blue - it's
     not "information," it's "nothing to do here." Accent (focus/chrome,
     not a status at all) sits in violet, not blue - focus-ring blue would
     sit close enough to ambient-status blue to risk reading as a fourth
     status color instead of pure chrome. */
  /* Every color-ish value the page uses is a flat, named token here - none
     computed inline (no color-mix()/filter() at the point of use) - so
     retuning any one of them is a single-line edit in exactly one of these
     two blocks, never a hunt through selectors for a magic number. */
  :root {
    --ground: #f5f6f8;
    --panel: #ffffff;
    --border: #dde1e7;
    --ink: #191c20;
    --ink-muted: #5c6570;
    --ink-faint: #98a1ab;
    --accent: #7c3aed;
    --ambient-fg: #3d6a8a;  --ambient-bg: #dcebf4;
    --ready-fg: #2d6b48;    --ready-bg: #dbeee1;
    --warn-fg: #8a4a0f;     --warn-bg: #fbe6cc;
    --block-fg: #ad2f28;    --block-bg: #fadcd9;
    --kind-fg: #565f68;     --kind-bg: #eaedf0;
    --shadow-color: rgba(20, 24, 30, 0.05);
    --shadow-color-strong: rgba(20, 24, 30, 0.07);
    --shadow: 0 1px 2px var(--shadow-color), 0 8px 24px var(--shadow-color-strong);
    /* .lane.is-cyclable's border and .lane:not(.is-cyclable)'s background -
       previously color-mix() calls against --ink-muted/--panel/--ground at
       the point of use; now their own settable colors. */
    --lane-cyclable-border: #939ba5;
    --lane-noncyclable-bg: #eff0f3;
    /* .signal:hover's filter: brightness() multiplier - a plain number
       (not a color), kept as its own token per mode so the light/dark
       split lives here instead of inside a nested @media block on the
       selector itself. */
    --signal-hover-brightness: 0.93;
    /* .signal:not(.is-cyclable)'s dimming - was a bare 0.72 with no name
       near any of the other tunable constants. */
    --signal-dim-opacity: 0.72;
    /* Deliberately not overridden in dark mode below - a consistently dark
       scrim behind the modal regardless of theme, so overlay content pops
       the same way either way. */
    --backdrop: rgba(10, 10, 12, 0.55);
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --ground: #131417;
      --panel: #1a1c20;
      --border: #2d3036;
      --ink: #e7e9ec;
      --ink-muted: #8e969f;
      --ink-faint: #565c64;
      --accent: #b794f6;
      --ambient-fg: #7bb3d9;  --ambient-bg: #182a38;
      --ready-fg: #16311f;    --ready-bg: #7ec49b;
      --warn-fg: #f0a94e;     --warn-bg: #3d2a10;
      --block-fg: #ef7b71;    --block-bg: #3a1714;
      --kind-fg: #a3adb8;     --kind-bg: #262a30;
      --shadow-color: rgba(0, 0, 0, 0.3);
      --shadow-color-strong: rgba(0, 0, 0, 0.4);
      --shadow: 0 1px 2px var(--shadow-color), 0 8px 24px var(--shadow-color-strong);
      --lane-cyclable-border: #454a52;
      --lane-noncyclable-bg: #1c1e22;
      --signal-hover-brightness: 1.18;
      --signal-dim-opacity: 0.72;
    }
  }

  :global(html, body) {
    background: transparent;
    color: var(--ink);
    font-family: ui-monospace, "SF Mono", "Cascadia Code", "Roboto Mono", Menlo, monospace;
    cursor: default;
    /* This is a fixed custom window, not a scrollable page - it should
       never show a scrollbar. */
    overflow: hidden;
  }
  :global(*) { cursor: inherit; }

  .dashboard {
    background: var(--ground);
    border-radius: 12px;
    padding: 0.6rem;
  }

  .panel {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    /* No visible background/border of its own - this exists so the
       pulse-noop outline below (which follows the element's own
       border-radius) comes out rounded instead of a hard square. */
    border-radius: 8px;
  }
  /* Acknowledges a show-inactive keypress that had no effect (edit mode was
     on - see ToggleShowInactive). A brief outline pulse, not a banner or
     system alert - same restrained language as the focus ring, offset
     outward so it doesn't sit flush against the lane rows' own borders. */
  .panel.pulse-noop {
    outline: 2px solid transparent;
    outline-offset: 6px;
    animation: pulse-noop 300ms ease;
  }
  @keyframes pulse-noop {
    0% { outline-color: transparent; }
    40% { outline-color: var(--accent); }
    100% { outline-color: transparent; }
  }

  .lane {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow);
    padding: 0.55rem 0.7rem;
    cursor: pointer;
    transition: border-color 0.1s ease;
  }
  .lane-body { flex: 1; min-width: 0; }

  /* Edit mode's only visible change - a toggle switch per row for the
     existing Lane.active flag (same one `lanes activate`/`deactivate`
     already flip), mounted only while editMode is on. */
  .lane-toggle {
    flex: none;
    width: 26px;
    height: 16px;
    border-radius: 999px;
    background: var(--border);
    position: relative;
    border: none;
    padding: 0;
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .lane-toggle::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--panel);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
    transition: transform 0.15s ease;
  }
  .lane-toggle.is-on { background: var(--accent); }
  .lane-toggle.is-on::after { transform: translateX(10px); }
  /* Cyclable lanes (from lib.rs's lane_cyclable - active, reachable, and
     hosting a live Claude session) get a stronger neutral border even
     unfocused, so a glance at the stack shows which lanes `sessions next`/
     `prev` would actually visit. Deliberately not "has any signal at all":
     an inactive lane, or one with only a pending-commit signal and no
     Claude session, has nothing for a cycle to land on and should read the
     same as an empty lane, not as a target. Kept neutral rather than
     status-colored, since a cyclable lane can still hold a mix of ready and
     blocking signals and tinting toward either would misreport the other.
     Session-missing isn't a border treatment at all - it's just another
     signal chip (kind: lanes), so it composes with this for free instead of
     needing its own border color to fight over. */
  .lane.is-cyclable { border-color: var(--lane-cyclable-border); }
  /* Non-cyclable lanes recede with a slightly greyed background, the
     inverse signal of is-cyclable's stronger border - together the two
     rules make "would a cycle land here" readable at a glance without
     resorting to a status color. */
  .lane:not(.is-cyclable) { background: var(--lane-noncyclable-bg); }
  /* Hover only ever fires once the window is actually key - macOS doesn't
     deliver mouse-move events to a non-key window at all (see the
     mousedown/click focus-guard in <script>), so this never has to account
     for an unfocused state; by the time a hover could show, the window is
     already focused. Declared after .lane.is-cyclable/:not(.is-cyclable) -
     same specificity (0,2,0) as either, so source order alone decides the
     tie, and this needs to win on both. Excludes is-unreachable (via
     :not()) - clicking one of these just opens an explanatory overlay, not
     a real action (see unreachableSignal in <script>), so it shouldn't
     invite the same "this is a live target" affordance normal lanes get. */
  .lane:hover:not(.is-unreachable) { border-color: var(--accent); }
  /* Focus surrounds whatever border is already there (Trello-style) rather
     than replacing it - a box-shadow ring sitting just outside the lane's
     own edge, so "focused" and "cyclable" stay two legible facts at once
     instead of one color winning over the other. */
  .lane.is-focused { box-shadow: 0 0 0 2px var(--accent), var(--shadow); }

  .lane-head {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    margin-bottom: 0.35rem;
  }
  .lane-name {
    font-size: 0.8rem;
    font-weight: 600;
    letter-spacing: -0.005em;
    color: var(--ink);
  }
  .lane-empty {
    font-size: 0.68rem;
    color: var(--ink-faint);
  }

  .signals {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
  }
  /* Extra breathing room once each chip can grow a session-toggle beside
     it - without this, adjacent chip+toggle pairs sit close enough to
     misread as one group. */
  .signals.is-editing {
    gap: 0.6rem;
  }
  .signal-wrap {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
  }
  /* Edit mode's per-session cycling toggle - only ever rendered next to a
     claude_session-kind chip (see the template), since that's the only
     signal kind cycling ever visits in the first place. Same round-switch
     language as .lane-toggle, just scaled down to sit inline with a chip
     instead of a whole row. */
  .session-toggle {
    flex: none;
    width: 26px;
    height: 16px;
    border-radius: 999px;
    background: var(--border);
    position: relative;
    border: none;
    padding: 0;
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .session-toggle::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--panel);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
    transition: transform 0.15s ease;
  }
  .session-toggle.is-on { background: var(--accent); }
  .session-toggle.is-on::after { transform: translateX(10px); }
  .signal {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font: inherit;
    font-size: 0.68rem;
    padding: 3px 7px 3px 4px;
    border: none;
    border-radius: 5px;
    line-height: 1.3;
    text-align: left;
    cursor: pointer;
    filter: brightness(1);
    transition: filter 0.1s ease;
  }
  /* A chip sits on its own urgency-colored fill, so the lane's border-color
     hover trick doesn't apply here - darkening/lightening the fill itself
     (brightness, not opacity, so it stays visually distinct from the
     is-cyclable dimming below) reads as "clickable" without introducing a
     fourth color into a chip that already carries kind + urgency. Same
     "only fires once the window is key" note as .lane:hover applies here
     too - see the focus-guard in <script>. Excludes is-unreachable, same
     reasoning as .lane:hover above - this chip just opens an explanatory
     overlay, not a real action. */
  .signal:hover:not(.is-unreachable) { filter: brightness(var(--signal-hover-brightness)); }
  /* The one signal chip that IS the currently-focused Claude session,
     distinct from "this lane is focused" - a lane can be focused with
     several live Claude sessions in it, only one of which is the one
     you're actually on. Same rule as the lane-level focus ring: surrounds
     the chip via box-shadow rather than replacing its own border, so it
     never has to fight the chip's own urgency-colored background. */
  .signal.is-active { box-shadow: 0 0 0 2px var(--accent); }
  /* Same "would a cycle land here" question as .lane.is-cyclable, one level
     down - cycling only ever visits live Claude sessions (signal.cyclable,
     from lib.rs's signal_cyclable()), so a pending-commit or
     session-missing chip recedes slightly rather than looking like
     something hypo+J/K would jump to. Opacity, not a background swap like
     the lane-level treatment: these chips still need their full
     urgency color to read (a non-cyclable blocking signal is still
     blocking), just muted enough to read as "not a cycle target." */
  .signal:not(.is-cyclable) { opacity: var(--signal-dim-opacity); }
  .signal .kind {
    font-size: 0.6rem;
    font-weight: 700;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--kind-fg);
    background: var(--kind-bg);
    border-radius: 3px;
    padding: 1px 5px;
  }
  .signal.urgency-info { background: var(--ambient-bg); color: var(--ambient-fg); }
  .signal.urgency-attention { background: var(--ready-bg); color: var(--ready-fg); }
  .signal.urgency-warning { background: var(--warn-bg); color: var(--warn-fg); }
  .signal.urgency-blocking { background: var(--block-bg); color: var(--block-fg); }

  .backdrop {
    position: fixed;
    inset: 0;
    background: var(--backdrop);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 10;
  }

  .overlay {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
    padding: 1.25rem;
    min-width: 240px;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .overlay-lane {
    font-size: 0.66rem;
    font-weight: 700;
    color: var(--ink-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .overlay-reason {
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--accent);
  }

  .overlay-detail {
    font-size: 0.74rem;
    color: var(--ink);
    line-height: 1.5;
  }
  .overlay-action {
    font-size: 0.68rem;
    color: var(--ink-muted);
    word-break: break-all;
  }
  .overlay-status {
    font-size: 0.68rem;
    color: var(--block-fg);
    word-break: break-all;
  }

  .overlay-buttons {
    margin-top: 0.4rem;
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .overlay-dismiss, .overlay-copy {
    font: inherit;
    font-size: 0.68rem;
    color: var(--ink-muted);
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 3px 10px;
    cursor: pointer;
    transition: border-color 0.1s, color 0.1s;
  }
  .overlay-copy {
    padding: 3px 8px;
    font-size: 0.8rem;
    line-height: 1;
  }
  .overlay-dismiss:hover, .overlay-copy:hover {
    border-color: var(--accent);
    color: var(--ink);
  }
</style>
