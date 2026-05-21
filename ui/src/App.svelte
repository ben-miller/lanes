<script>
  import { openUrl } from "@tauri-apps/plugin-opener";

  const projects = [
    {
      name: "Sheetwork - Weekly View",
      url: "obsidian://open?vault=Dev&file=Topics%2FSoftware%20Engineering%2FProjects%2FSheetwork%2FAtlas%2FSheetwork%20-%20Weekly%20View",
    },
    {
      name: "Sheetwork - Project Overview",
      url: "obsidian://open?vault=Dev&file=Topics%2FSoftware%20Engineering%2FProjects%2FSheetwork%2FSheetwork%20-%20Project%20Overview",
    },
  ];
</script>

<main>
  <h1>Lanes</h1>
  <ul>
    {#each projects as project}
      <li>
        <button on:click={() => openUrl(project.url)}>
          <strong>{project.name}</strong>
        </button>
      </li>
    {/each}
  </ul>
</main>

<style>
  main {
    font-family: sans-serif;
    max-width: 480px;
    margin: 2rem auto;
    padding: 0 1rem;
  }

  h1 {
    font-size: 1.5rem;
    margin-bottom: 1rem;
  }

  ul {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  li {
    margin: 0.5rem 0;
  }

  button {
    display: flex;
    gap: 1rem;
    align-items: baseline;
    width: 100%;
    text-align: left;
    padding: 0.75rem 1rem;
    background: #f4f4f4;
    border: 1px solid #ddd;
    border-radius: 6px;
    cursor: pointer;
    font-size: 1rem;
  }

  button:hover {
    background: #e8e8e8;
  }
</style>
