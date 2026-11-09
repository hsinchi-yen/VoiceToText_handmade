<script>
  // Per-app injection rules ([[apps]]): newline key and always-paste.
  let { view, save } = $props();

  const rules = $derived(view.config.apps);
  let newExe = $state("");

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

<section class="settings-section">
  <h2>App 規則</h2>
  <p class="hint">
    聊天程式按 Enter 會送出訊息，換行要用 Shift+Enter；虛擬機視窗收不到模擬打字，要改用貼上。
    名稱是執行檔名稱（可在工作管理員 → 詳細資料查到）。
  </p>
  <table>
    <thead>
      <tr><th>App</th><th>換行鍵</th><th>一律貼上</th><th></th></tr>
    </thead>
    <tbody>
      {#each rules as r, i (r.exe)}
        <tr>
          <td>{r.exe}</td>
          <td>
            <select value={r.newline} onchange={(e) => update(i, { newline: e.currentTarget.value })}>
              <option value="enter">Enter</option>
              <option value="shift_enter">Shift+Enter</option>
            </select>
          </td>
          <td>
            <input type="checkbox" checked={r.always_paste} aria-label="一律貼上"
              onchange={(e) => update(i, { always_paste: e.currentTarget.checked })} />
          </td>
          <td>
            <button disabled={rules.length <= 1} onclick={() => saveRules(rules.filter((_, j) => j !== i))}>
              移除
            </button>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  <p>
    <input type="text" placeholder="例如 Code.exe" bind:value={newExe} spellcheck="false"
      onkeydown={(e) => e.key === "Enter" && add()} />
    <button onclick={add}>新增規則</button>
  </p>
</section>

<style>
  table {
    border-collapse: collapse;
    width: 100%;
  }
  th,
  td {
    text-align: left;
    padding: 0.2rem 0.4rem;
    border-bottom: 1px solid color-mix(in srgb, currentColor 10%, transparent);
  }
</style>
