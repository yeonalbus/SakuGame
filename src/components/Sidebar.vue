<script setup lang="ts">
defineProps<{ active: string }>();
const emit = defineEmits<{ navigate: [key: string] }>();
</script>

<template>
  <aside class="sidebar">
    <div class="brand">SakuGame</div>
    <button
      class="nav-item"
      :class="{ active: active === 'home' }"
      @click="emit('navigate', 'home')"
    >
      主页
    </button>
    <button
      class="nav-item"
      :class="{ active: active === 'transfer' }"
      @click="emit('navigate', 'transfer')"
    >
      搬运
    </button>
    <button
      class="nav-item"
      :class="{ active: active === 'settings' }"
      @click="emit('navigate', 'settings')"
    >
      设置
    </button>
  </aside>
</template>
