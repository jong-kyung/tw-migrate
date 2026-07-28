<template>
  <p class="card">First</p>
  <p class="plain">Second</p>
</template>
<script setup>
const answer = 42
</script>
<style scoped>
.card {
  padding: 13px;
}
@media (min-width: 640px) {
  .card {
    padding: 17px;
  }
}
</style>
