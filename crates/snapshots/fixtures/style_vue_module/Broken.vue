<template>
  <p :class="$style.tone">First</p>
  <p :class="cond ? $style.tone : ''">Second</p>
</template>
<script setup>
const cond = true;
</script>
<style module>
.tone {
  color: red;
}
</style>
