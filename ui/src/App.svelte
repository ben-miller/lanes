<script>
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount, onDestroy } from "svelte";

  let snapshot = null;
  let activeSignal = null;

  async function refresh() {
    snapshot = await invoke("get_snapshot");
  }

  let timer;
  onMount(() => {
    refresh();
    timer = setInterval(refresh, 3000);
    document.addEventListener("mousedown", (e) => {
      if (!e.target.closest(".signal") && !e.target.closest(".overlay")) {
        getCurrentWindow().startDragging();
      }
    });
  });
  onDestroy(() => clearInterval(timer));

  function allSignals(lane) {
    return lane.facets.flatMap(f => f.signals ?? []);
  }

  function signalLabel(signal) {
    if (signal.reason === "pending_commit") return "pending commit";
    if (signal.reason === "claude_session_active") return "claude · running";
    if (signal.reason === "claude_session_awaiting") return "claude · waiting";
    return signal.reason;
  }

  async function handleSignalClick(lane, signal) {
    if (signal.action) {
      const err = await invoke("execute_action", { action: signal.action }).then(() => null).catch(e => String(e));
      if (err) activeSignal = { lane, signal, status: err };
    } else {
      activeSignal = { lane, signal, status: null };
    }
  }

  function dismissOverlay() {
    activeSignal = null;
  }

  function handleKeydown(e) {
    if (e.key === "Escape") dismissOverlay();
  }
</script>

<svelte:window on:keydown={handleKeydown} />

{#if snapshot}
  <div class="dashboard">
    <div class="lanes">
      {#each snapshot.lanes as lane}
        {@const signals = allSignals(lane)}
        <div class="column" class:has-signals={signals.length > 0}>
          <span class="name">{lane.name ?? lane.id}</span>
          {#each signals as signal}
            <button
              class="signal"
              on:mousedown|stopPropagation={() => handleSignalClick(lane, signal)}
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
      <div class="overlay-lane">{activeSignal.lane.name ?? activeSignal.lane.id}</div>
      <div class="overlay-reason">{signalLabel(activeSignal.signal)}</div>
      {#if activeSignal.signal.action}
        <div class="overlay-action">{JSON.stringify(activeSignal.signal.action)}</div>
      {:else}
        <div class="overlay-action">no action</div>
      {/if}
      {#if activeSignal.status}
        <div class="overlay-status" class:ok={activeSignal.status === 'ok'}>{activeSignal.status}</div>
      {/if}
      <button class="overlay-dismiss" on:click={dismissOverlay}>dismiss</button>
    </div>
  </div>
{/if}

<style>
  :global(*, *::before, *::after) { box-sizing: border-box; margin: 0; padding: 0; }
  :global(body) { background: #111; color: #e0e0e0; font-family: system-ui, sans-serif; }

  .dashboard {
    height: 100vh;
    padding: 2rem 1rem 1rem;
  }

  .lanes {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    align-items: flex-start;
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

  .overlay-dismiss {
    margin-top: 0.4rem;
    align-self: flex-end;
    font-size: 0.72rem;
    color: #888;
    background: none;
    border: 1px solid #333;
    border-radius: 4px;
    padding: 3px 10px;
    cursor: pointer;
    transition: border-color 0.1s, color 0.1s;
  }
  .overlay-dismiss:hover {
    border-color: #888;
    color: #e0e0e0;
  }
</style>
