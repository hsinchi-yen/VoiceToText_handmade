<script>
  // Only offered once a cloud service exists.
  import { Switch } from "../lib/index.js";
  import Row from "./Row.svelte";

  let { view, save } = $props();
</script>

<Row dirty={view.dirty?.has("cloud_fallback_to_local")}>
  <Switch checked={view.config.route.cloud_fallback_to_local} label="雲端失敗時自動改用地端"
    onchange={(e) => save([{ field: "cloud_fallback_to_local", value: e.currentTarget.checked }])} />
</Row>
