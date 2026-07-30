<template>
  <div :class="'base'">Literal</div>
  <span :class="`active`">Sibling</span>
</template>
<style scoped>
.base {
  padding: 11px;
}
.active {
  margin: 7px;
}
</style>
