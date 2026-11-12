<script>
  import { isLinux } from "../platform.js";
  import { Button, IconButton, Select, Switch, TextField } from "../lib/index.js";
  import Row from "./Row.svelte";
  // Per-app injection rules ([[apps]]): newline key and always-paste.
  let { view, save } = $props();

  const rules = $derived(view.config.apps);
  let newExe = $state("");
  const newlineOptions = [
    { value: "enter", label: "Enter" },
    { value: "shift_enter", label: "Shift+Enter" },
  ];

  function saveRules(next) {
    return save([{ field: "apps", value: next }]);
  }

  function update(i, patch) {
    saveRules(rules.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  }

  async function add() {
    const exe = newExe.trim();
    if (!exe) return;
    if (await saveRules([...rules, { exe, newline: "enter", always_paste: false }])) newExe = "";
  }
</script>

<Row dirty={view.dirty?.has("apps")}>
  <p class="hint">
    聊天程式按 Enter 會送出訊息，換行要用 Shift+Enter；虛擬機視窗收不到模擬打字，要改用貼上。
    {#if isLinux}
      Linux（GNOME Wayland）目前無法得知前景程式，這些規則暫時不會生效，換行一律用 Enter。
    {:else}
      名稱是執行檔名稱（可在工作管理員 → 詳細資料查到）。
    {/if}
  </p>
  <ul>
    {#each rules as r, i (r.exe)}
      <li>
        <b class="exe">{r.exe}</b>
        <div class="sel">
          <Select label="換行鍵" value={r.newline} options={newlineOptions}
            onchange={(e) => update(i, { newline: e.currentTarget.value })} />
        </div>
        <Switch checked={r.always_paste} label="一律貼上"
          onchange={(e) => update(i, { always_paste: e.currentTarget.checked })} />
        <IconButton icon="trash" label={`移除 ${r.exe}`} disabled={rules.length <= 1}
          onclick={() => saveRules(rules.filter((_, j) => j !== i))} />
      </li>
    {/each}
  </ul>
  <div class="add">
    <TextField label="新增 App" placeholder="例如 Code.exe" bind:value={newExe} spellcheck="false"
      onkeydown={(e) => e.key === "Enter" && add()} />
    <Button variant="tonal" icon="plus" onclick={add}>新增規則</Button>
  </div>
</Row>

<style>
  p {
    margin: 0 0 var(--md-space-2);
  }
  ul {
    display: grid;
    gap: var(--md-space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--md-space-2) var(--md-space-4);
    padding: var(--md-space-2) 0;
    border-bottom: 1px solid var(--md-outline-variant);
  }
  .exe {
    flex: 1 1 8rem;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .sel {
    width: 11rem;
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--md-space-3);
    margin-top: var(--md-space-3);
  }
  .add :global(.field) {
    flex: 1 1 12rem;
  }
</style>
