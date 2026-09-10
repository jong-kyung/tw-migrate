<script setup>
import Child from './Child.vue';
</script>
<template>
  <article class="card" data-probe="card" data-identity="card">Card</article>
  <button class="action" data-identity="action">Toggle details</button>
  <section class="responsive" data-probe="responsive-layout" data-identity="responsive">Responsive</section>
  <aside :class="$style.panel" data-probe="module-panel" data-identity="panel">Panel</aside>
  <Child />
</template>
<style scoped lang="scss">
$space: 13px;
.card {
  padding: $space;
  color: rgb(20, 30, 40);
}
.card:hover {
  background-color: rgb(210, 220, 230);
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
.child-root {
  padding: $space + 4px;
}
.retained {
  display: block;
}
</style>
<style module>
.panel {
  margin: 6px;
  border-left: 4px solid rgb(60, 120, 60);
}
</style>
