<script>
  let count = $state(1);
  let doubled = $derived(count * 2);
</script>
<button onclick={() => count++}>{count} / {doubled}</button>
