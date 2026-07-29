<script setup>
import { ref } from "vue";

const active = ref(false);
</script>
<template>
  <article :class="['card', { active }]" data-probe="card" data-identity="card">Card</article>
  <button class="action" data-identity="action" @focus="active = true">Toggle details</button>
  <section class="responsive" data-probe="responsive-layout" data-identity="responsive">Responsive</section>
</template>
<style scoped>
.card {
  padding: 13px;
  color: rgb(20, 30, 40);
}
.card:hover {
  background-color: rgb(210, 220, 230);
}
.active {
  margin-left: 19px;
}
.action:focus {
  outline: 2px solid rgb(30, 90, 150);
}
.action:focus-visible {
  outline-offset: 3px;
}
.responsive {
  width: 100px;
}
@media (min-width: 700px) {
  .responsive {
    width: 200px;
  }
}
.retained {
  display: block;
}
</style>
