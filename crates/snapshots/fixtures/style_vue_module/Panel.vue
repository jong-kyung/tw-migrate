<template>
  <p :class="$style.card">First</p>
  <p class="plain" :class="$style.note">Second</p>
</template>
<style module>
.card {
  padding: 13px;
}
.note {
  margin: 7px;
}
</style>
