<script lang="ts">
  let { count }: { count: number } = $props();
</script>
<p>{count.toFixed(0)}</p>
