<script>
  function label(node) { node.setAttribute('title', 'attached'); return () => node.removeAttribute('title'); }
</script>
<button {@attach label}>Attached</button>
