<script lang="ts">
  let { choice = 'second' }: { choice?: string } = $props();
</script>
<select defaultValue={choice} aria-label="Choice">
  <option value="first">First</option>
  <option value="second">Second</option>
</select>
