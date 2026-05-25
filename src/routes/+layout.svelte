<script lang="ts">
  import "../app.css";
  import { init } from "$lib/theme.svelte";

  let { children } = $props();

  $effect(() => {
    init();
  });
</script>

{@render children()}
