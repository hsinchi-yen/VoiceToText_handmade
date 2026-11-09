<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  // Error codes from src-tauri/src/commands/history.rs. Never contain user text.
  const errorText = {
    history_unavailable: "無法開啟輸入紀錄",
    history_failed: "讀寫輸入紀錄失敗",
    history_not_found: "這筆紀錄已不存在",
    clipboard_failed: "無法存取剪貼簿",
    window_failed: "無法切換視窗",
    no_target_window: "找不到要輸入的視窗：請先點一下要輸入的視窗，再回來按「重新輸出」",
    forbidden: "只能在主視窗操作輸入紀錄",
    inject_elevated: "目標視窗以系統管理員身分執行，無法輸入（文字已複製到剪貼簿）",
    inject_partial: "只輸入了部分文字",
    inject_paste_unsupported: "此 App 需要貼上模式（尚未支援）",
    inject_modifiers_held: "修飾鍵未放開，已取消輸入",
    inject_failed: "輸入失敗",
    platform_unsupported: "此平台尚未支援",
  };

  let items = $state([]);
  let loaded = $state(false);
  let message = $state("");
  let confirmClear = $state(false);

  const fmt = new Intl.DateTimeFormat("zh-TW", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });

  function describe(code) {
    return errorText[code] ?? `錯誤：${code}`;
  }

  async function refresh() {
    try {
      items = await invoke("history_list");
    } catch (code) {
      message = describe(code);
    }
    loaded = true;
  }

  async function act(command, id, done) {
    message = "";
    try {
      await invoke(command, id === undefined ? {} : { id });
      if (done) message = done;
    } catch (code) {
      message = describe(code);
    }
    await refresh();
  }

  const copy = (id) => act("history_copy", id, "已複製到剪貼簿");
  const reoutput = (id) => act("history_reoutput", id, "已重新輸出");
  const remove = (id) => act("history_delete", id, "已刪除");
  async function clearAll() {
    confirmClear = false;
    await act("history_clear", undefined, "已清除全部紀錄");
  }

  $effect(() => {
    let unlisten;
    refresh();
    // A finished utterance may have added an entry.
    listen("v2c:status", (e) => {
      if (e.payload?.kind === "done" || e.payload?.kind === "error") refresh();
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  });
</script>

<h1>輸入紀錄</h1>
<p class="hint">
  最近的輸入結果（預設最多 200 筆、保留 7 天後自動刪除）。內容已加密保存在本機；密碼管理器等「不記錄的 App」不會留下紀錄。
</p>

<div class="toolbar">
  {#if confirmClear}
    <span>確定要清除全部 {items.length} 筆紀錄？此動作無法復原。</span>
    <button class="danger" onclick={clearAll}>確定清除</button>
    <button onclick={() => (confirmClear = false)}>取消</button>
  {:else}
    <button class="danger" disabled={items.length === 0} onclick={() => (confirmClear = true)}>
      全部清除
    </button>
  {/if}
  {#if message}<span class="message" role="status">{message}</span>{/if}
</div>

{#if loaded && items.length === 0}
  <p class="empty">目前沒有紀錄。</p>
{/if}

<ul>
  {#each items as item (item.id)}
    <li>
      <div class="meta">
        <span>{fmt.format(new Date(item.created_at * 1000))}</span>
        <span>{item.app || "未知 App"}</span>
        <span>{item.chars} 字</span>
        {#if item.mode === "translate"}<span class="tag">中翻英</span>{/if}
      </div>
      <p class="text">{item.text}</p>
      <div class="actions">
        <button onclick={() => copy(item.id)}>複製</button>
        <button onclick={() => reoutput(item.id)} title="切回上一個視窗並輸入這段文字">
          重新輸出
        </button>
        <button onclick={() => remove(item.id)}>刪除</button>
      </div>
    </li>
  {/each}
</ul>

<style>
  .hint,
  .empty {
    opacity: 0.7;
    font-size: 0.9rem;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
    margin: 0.75rem 0;
  }
  .message {
    opacity: 0.85;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  li {
    padding: 0.6rem 0;
    border-bottom: 1px solid color-mix(in srgb, currentColor 15%, transparent);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    font-size: 0.8rem;
    opacity: 0.65;
  }
  .tag {
    color: #2f81f7;
    opacity: 1;
  }
  .text {
    margin: 0.3rem 0;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  button.danger {
    color: #cf222e;
  }
</style>
