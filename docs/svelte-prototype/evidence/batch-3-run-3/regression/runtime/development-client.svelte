<script lang="ts">
  let { step = 1, label = 'Count' }: { step?: number; label?: string } = $props();
  let count = $state(0);
  let doubled = $derived(count * 2);
</script>
<button type="button" onclick={() => count += step}>{label}: {count} / {doubled}</button>
