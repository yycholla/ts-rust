<script module lang="ts">
  export const prefix: string = 'module';
</script>
<script lang="ts">
  let value = $state<number>(3);
</script>
<p>{prefix}: {value}</p>
