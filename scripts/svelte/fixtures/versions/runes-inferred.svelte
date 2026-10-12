<script>let count = $state(1); const doubled = $derived(count * 2);</script>
<button onclick={() => count++}>{doubled}</button>
