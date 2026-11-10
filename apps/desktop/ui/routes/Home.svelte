<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  // Status codes from src-tauri/src/controller.rs (StatusEvent). Never contains user text.
  const errorText = {
    model_missing: "還沒有語音模型：請到「初次設定」下載",
    model_load_failed: "語音模型載入失敗",
    asr_loading: "語音模型載入中，請稍候",
    asr_switching: "切換語音模型中，這段語音會用新模型辨識",
    sensevoice_not_available: "SenseVoice 模型無法載入（請到「初次設定」重新下載）",
    not_configured: "未設定翻譯模型",
    inject_elevated: "目前視窗以系統管理員身分執行，無法輸入",
    inject_paste_unsupported: "此 App 需要貼上模式（尚未支援）",
    inject_partial: "只輸入了部分文字",
    inject_modifiers_held: "修飾鍵未放開，已取消輸入",
    hotkey_failed: "熱鍵安裝失敗",
    mic_not_found: "找不到麥克風",
    mic_lost: "麥克風中斷，正在重新連線",
    config_invalid: "設定檔格式錯誤，已使用預設值",
    busy: "處理中，這段語音已略過",
  };

  let info = $state(null);
  let status = $state(null);

  function describe(s) {
    if (!s) return "待命中：按住右 Ctrl 說話";
    switch (s.kind) {
      case "recording":
        return "錄音中…";
      case "transcribing":
        return "辨識中…";
      case "polishing":
        return "潤飾中…";
      case "done":
        return s.chars > 0
          ? `完成：${s.chars} 字，放開到出字 ${s.latency_ms} ms`
          : "沒有偵測到語音";
      case "cancelled":
        return "已取消";
      case "mic_ready":
        return "麥克風已重新連線";
      case "error":
        return `錯誤：${errorText[s.code] ?? s.code}`;
      default:
        return s.kind;
    }
  }

  $effect(() => {
    let unlisten;
    invoke("app_info").then((i) => (info = i)).catch(() => {});
    invoke("last_status").then((s) => (status = s ?? status)).catch(() => {});
    listen("v2c:status", (e) => (status = e.payload)).then((u) => (unlisten = u));
    return () => unlisten?.();
  });

  const level = $derived(status?.kind === "recording" ? Math.min(1, status.rms * 8) : 0);
</script>

<h1>v2c 語音輸入</h1>
<p class="status">{describe(status)}</p>
{#if status?.kind === "error" && (status.code === "model_missing" || status.code === "sensevoice_not_available")}
  <p><a href="#/onboarding">前往初次設定</a></p>
{/if}
<div class="meter"><div style:width={`${level * 100}%`}></div></div>

<ul>
  <li>按住右 Ctrl 說話，放開後文字會輸入到游標位置。</li>
  <li>Shift + 右 Ctrl：說中文、輸出英文（需要翻譯模型）。</li>
  <li>Esc：取消這段錄音。</li>
</ul>

{#if info}
  <p class="info">
    版本 {info.version}・CUDA {info.cuda ? "已啟用" : "未啟用"}・資料夾 {info.data_dir}
  </p>
{/if}

<style>
  .status {
    font-size: 1.2rem;
  }
  .meter {
    height: 6px;
    background: color-mix(in srgb, currentColor 15%, transparent);
    border-radius: 3px;
    overflow: hidden;
  }
  .meter div {
    height: 100%;
    background: #2f81f7;
    transition: width 50ms linear;
  }
  .info {
    opacity: 0.6;
    font-size: 0.85rem;
  }
</style>
