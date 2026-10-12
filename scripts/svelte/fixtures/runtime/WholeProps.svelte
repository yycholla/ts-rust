<script>
  let props = $props();
  let count = $state(0);
</script>
<button {...props} type="button" onclick={() => count++}>{props.named}:{props.title}:{'named' in props}:{Object.keys(props).sort().join(',')}:{count}</button>
