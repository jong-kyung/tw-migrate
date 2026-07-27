<template>
  <ul class="list">First</ul>
  <li :class="tone" class="row">Second</li>
</template>
<script setup>
const tone = 'warm';
</script>
<style scoped>
.list {
  color: red;
}
</style>
