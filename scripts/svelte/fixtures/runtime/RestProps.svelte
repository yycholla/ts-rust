<script>
  let { named, ...rest } = $props();
  let count = $state(0);
</script>
<button {...rest} type="button" onclick={() => count++}>{named}:{rest.title}:{'named' in rest}:{Object.keys(rest).sort().join(',')}:{count}</button>
