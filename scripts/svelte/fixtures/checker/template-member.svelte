<script lang="ts">
  let { count }: { count: number } = $props();
</script>
<p>{count.notARealMember()}</p>
