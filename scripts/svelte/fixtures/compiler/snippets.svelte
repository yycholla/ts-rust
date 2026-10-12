<script lang="ts">
  let { label = 'world' }: { label?: string } = $props();
</script>
{#snippet greeting(value: string)}<strong>Hello {value}</strong>{/snippet}
<section>{@render greeting(label)}</section>
