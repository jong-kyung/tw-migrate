<template>
  <div :class="['base', { active: enabled }, wide ? 'wide' : 'narrow']">Literal</div>
  <span>Sibling</span>
</template>
<script setup>
const enabled = true;
const wide = true;
</script>
<style scoped>
.base {
  padding: 11px;
}
.active {
  margin: 7px;
}
.wide {
  gap: 5px;
}
.narrow {
  top: 3px;
}
</style>
