<script setup lang="ts">
import { ref } from "vue";
import Sidebar from "./components/Sidebar.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import HomeView from "./views/HomeView.vue";
import GameDetailView from "./views/GameDetailView.vue";
import SettingsView from "./views/SettingsView.vue";
import type { ViewKey } from "./types";

const view = ref<ViewKey>("home");
const selectedGameId = ref<number | null>(null);
const toast = ref<{ text: string; error: boolean } | null>(null);
let toastTimer: number | undefined;

function showToast(text: string, error = false) {
  toast.value = { text, error };
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toast.value = null;
  }, 3600);
}

function navigate(next: string) {
  view.value = next as ViewKey;
  if (next !== "detail") selectedGameId.value = null;
}

function openGame(id: number) {
  selectedGameId.value = id;
  view.value = "detail";
}
</script>

<template>
  <div class="app">
    <Sidebar
      :active="view === 'settings' ? 'settings' : 'home'"
      @navigate="navigate"
    />

    <!-- 上边栏：仅占位（DUI-01，v1 不做搜索） -->
    <header class="topbar" />

    <main class="main">
      <HomeView v-if="view === 'home'" @open="openGame" @toast="showToast" />
      <GameDetailView
        v-else-if="view === 'detail' && selectedGameId !== null"
        :game-id="selectedGameId"
        @back="navigate('home')"
        @toast="showToast"
      />
      <SettingsView v-else-if="view === 'settings'" @toast="showToast" />
    </main>

    <div v-if="toast" class="toast" :class="{ error: toast.error }">{{ toast.text }}</div>

    <!-- 全局确认对话框（替代 window.confirm） -->
    <ConfirmDialog />
  </div>
</template>
