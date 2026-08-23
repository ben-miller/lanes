<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow, LogicalSize, LogicalPosition, primaryMonitor } from "@tauri-apps/api/window";
  import { onMount, onDestroy } from "svelte";

  const MAX_COLS = 6;
  const COL_WIDTH = 180;
  const ROW_HEIGHT = 176; // matches .column height (11rem @ 16px root)
  const GAP = 12; // matches .lanes gap (0.75rem)
  const PADDING = 16; // matches .dashboard padding (1rem)

  let snapshot = null;
  let activeSignal = null;
  let cols = 1;

  async function refresh() {
    snapshot = await invoke("get_snapshot");
    cols = Math.max(1, Math.min(snapshot.lanes.length, MAX_COLS));
    await resizeToContent(snapshot.lanes.length);
  }

  async function resizeToContent(laneCount) {
    const c = Math.max(1, Math.min(laneCount, MAX_COLS));
    const rows = Math.max(1, Math.ceil(laneCount / MAX_COLS));
    const width = c * COL_WIDTH + (c - 1) * GAP + PADDING * 2;
    const height = rows * ROW_HEIGHT + (rows - 1) * GAP + PADDING * 2;

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

  onMount(async () => {
    refresh();
    timer = setInterval(refresh, 10000);
    unlistenSessions = await listen("sessions-changed", () => refresh());
    // Fires immediately on a lane/session change (see lib.rs) - update both
    // highlights right away instead of waiting on the slower full refresh
    // above, same as the optimistic local update already done for in-UI
    // signal clicks.
    unlistenLane = await listen("lane-changed", (event) => {
      if (snapshot) snapshot = { ...snapshot, current_lane: event.payload.lane, current_claude_session: event.payload.session };
    });
    document.addEventListener("mousedown", (e) => {
      if (!e.target.closest(".signal") && !e.target.closest(".overlay")) {
        getCurrentWindow().startDragging();
      }
    });
  });
  onDestroy(() => {
    clearInterval(timer);
    if (unlistenSessions) unlistenSessions();
    if (unlistenLane) unlistenLane();
  });

  function allSignals(lane) {
    return lane.facets.flatMap(f => f.signals ?? []);
  }

  function signalLabel(signal) {
    if (signal.reason === "pending_commit") return "pending commit";
    if (signal.reason === "claude_session_active") return "claude · running";
    if (signal.reason === "claude_session_awaiting") return "claude · idle";
    if (signal.reason === "claude_session_permission") return "claude · permission";
    return signal.reason;
  }

  async function handleLaneClick(lane) {
    snapshot = { ...snapshot, current_lane: lane.id };
    await invoke("activate_lane", { laneId: lane.id });
    await refresh();
  }

  async function handleSignalClick(lane, signal) {
    const sessionId = signal.action?.kind === "switch_claude_session" ? signal.action.session_id : snapshot.current_claude_session;
    snapshot = { ...snapshot, current_lane: lane.id, current_claude_session: sessionId };
    await invoke("set_current_lane", { laneId: lane.id });
    if (signal.action) {
      const err = await invoke("execute_action", { action: signal.action }).then(() => null).catch(e => String(e));
      if (err) activeSignal = { lane, signal, status: err };
    } else {
      activeSignal = { lane, signal, status: null };
    }
    await refresh();
  }

  function dismissOverlay() {
    activeSignal = null;
  }

  async function copyErrorReport() {
    const s = activeSignal;
    if (!s) return;
    const report = [
      `lane: ${s.lane.name}`,
      `signal: ${signalLabel(s.signal)}`,
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
    if (e.key === "Escape") dismissOverlay();
  }
</script>

<svelte:window on:keydown={handleKeydown} />

{#if snapshot}
  <div class="dashboard">
    <div class="lanes" style="--cols: {cols}">
      {#each snapshot.lanes as lane}
        {@const signals = allSignals(lane)}
        <div class="column" class:has-signals={signals.length > 0} class:current={snapshot.current_lane === lane.id} on:click={() => handleLaneClick(lane)}>
          <span class="name">{lane.name}</span>
          {#each signals as signal}
            <button
              class="signal"
              class:current-session={signal.action?.kind === "switch_claude_session" && signal.action.session_id === snapshot.current_claude_session}
              on:mousedown|stopPropagation={() => handleSignalClick(lane, signal)}
              on:click|stopPropagation
            >{signalLabel(signal)}</button>
          {/each}
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
      {#if activeSignal.signal.action}
        <div class="overlay-action">{JSON.stringify(activeSignal.signal.action)}</div>
      {:else}
        <div class="overlay-action">no action</div>
      {/if}
      {#if activeSignal.status}
        <div class="overlay-status" class:ok={activeSignal.status === 'ok'}>{activeSignal.status}</div>
      {/if}
      <div class="overlay-buttons">
        {#if activeSignal.status && activeSignal.status !== 'ok'}
          <button class="overlay-copy" title="copy error" on:click={copyErrorReport}>⧉</button>
        {/if}
        <button class="overlay-dismiss" on:click={dismissOverlay}>dismiss</button>
      </div>
    </div>
  </div>
{/if}

<style>
  :global(*, *::before, *::after) { box-sizing: border-box; margin: 0; padding: 0; user-select: none; }
  :global(html, body) { background: transparent; color: #e0e0e0; font-family: system-ui, sans-serif; cursor: default; }
  :global(*) { cursor: inherit; }

  .dashboard {
    background: #111;
    border-radius: 12px;
    padding: 1rem;
  }

  .lanes {
    display: grid;
    grid-template-columns: repeat(var(--cols), 180px);
    gap: 0.75rem;
  }

  .column {
    width: 180px;
    height: 11rem;
    overflow-y: auto;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 8px;
    padding: 0.6rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .column.has-signals { border-color: #7a5200; }
  .column.current { border-color: #e0e0e0; border-width: 2px; }
  .column.current.has-signals { border-color: #ffb347; border-width: 2px; }

  .name {
    font-size: 0.78rem;
    font-weight: 600;
    color: #e0e0e0;
    padding: 0 0.2rem 0.2rem;
    border-bottom: 1px solid #2a2a2a;
    margin-bottom: 0.1rem;
  }

  .signal {
    font-size: 0.72rem;
    color: #ffb347;
    background: #7a5200;
    padding: 4px 8px;
    border-radius: 4px;
    border: 1px solid transparent;
    text-align: left;
    cursor: pointer;
  }
  .signal.current-session {
    border-color: #ffb347;
    border-width: 2px;
  }

  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 10;
  }

  .overlay {
    background: #1e1e1e;
    border: 1px solid #7a5200;
    border-radius: 10px;
    padding: 1.5rem;
    min-width: 240px;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .overlay-lane {
    font-size: 0.7rem;
    font-weight: 600;
    color: #888;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .overlay-reason {
    font-size: 1rem;
    font-weight: 600;
    color: #ffb347;
  }

  .overlay-action {
    font-size: 0.72rem;
    color: #666;
    font-family: monospace;
    word-break: break-all;
  }
  .overlay-status {
    font-size: 0.72rem;
    color: #e55;
    font-family: monospace;
    word-break: break-all;
  }
  .overlay-status.ok { color: #5a5; }

  .overlay-buttons {
    margin-top: 0.4rem;
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .overlay-dismiss, .overlay-copy {
    font-size: 0.72rem;
    color: #888;
    background: none;
    border: 1px solid #333;
    border-radius: 4px;
    padding: 3px 10px;
    cursor: pointer;
    transition: border-color 0.1s, color 0.1s;
  }
  .overlay-copy {
    padding: 3px 8px;
    font-size: 0.85rem;
    line-height: 1;
  }
  .overlay-dismiss:hover, .overlay-copy:hover {
    border-color: #888;
    color: #e0e0e0;
  }
</style>
