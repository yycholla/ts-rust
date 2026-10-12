<script lang="ts">
  let { count, name = 'visitor' }: { count: number; name?: string } = $props();
</script>
<svelte:head><title>Hello {name}</title></svelte:head>
<p>{name}: {count.toFixed(0)}</p>
