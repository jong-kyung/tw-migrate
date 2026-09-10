<template>
  <section class="child-root" data-probe="child-root" data-identity="child-root">
    <span class="child-root retained-child" data-probe="child-interior" data-identity="child-interior">Child</span>
  </section>
</template>
<style scoped>
.retained-child {
  padding: 3px;
}
:deep(.retained-child) {
  letter-spacing: 2px;
}
</style>
