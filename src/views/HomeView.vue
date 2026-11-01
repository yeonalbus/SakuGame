<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorText, onScanFinished } from "../api";
import GameCard from "../components/GameCard.vue";
import type { GameRow } from "../types";

const emit = defineEmits<{ open: [id: number]; toast: [text: string, error?: boolean] }>();

const games = ref<GameRow[]>([]);
const loading = ref(true);
let unlisten: UnlistenFn | null = null;

async function load() {
  loading.value = true;
  try {
    games.value = await api.listGames(null);
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    loading.value = false;
  }
}

onMounted(async () => {
  await load();
  unlisten = await onScanFinished(() => {
    void load();
  });
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <div class="empty" v-if="loading">加载中…</div>
  <div class="empty" v-else-if="games.length === 0">
    还没有游戏档案<br /><span class="faint">到「设置 → 仓库管理」添加仓库路径并扫描</span>
  </div>
  <div class="grid" v-else>
    <GameCard v-for="g in games" :key="g.id" :game="g" @open="emit('open', $event)" />
  </div>
</template>
