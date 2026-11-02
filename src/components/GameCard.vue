<script setup lang="ts">
import { ref, computed, onMounted, watch } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import type { GameRow } from "../types";

const props = defineProps<{ game: GameRow }>();
const emit = defineEmits<{ open: [id: number] }>();

const titleEl = ref<HTMLElement | null>(null);
const imgFailed = ref(false);

const missing = computed(() => props.game.status === "missing");
const badge = computed(() => (props.game.game_code ? props.game.game_code : null));

const coverSrc = computed(() => {
  if (!props.game.cover_path || imgFailed.value) return null;
  return convertFileSrc(props.game.cover_path);
});

/** DUI-02：长标题先缩字号，最小字号仍溢出则靠 CSS line-clamp 截断 */
function fitTitle() {
  const el = titleEl.value;
  if (!el) return;
  el.style.fontSize = "";
  for (let size = 13; size >= 11; size--) {
    el.style.fontSize = `${size}px`;
    if (el.scrollHeight <= el.clientHeight + 1) break;
  }
}

onMounted(fitTitle);
watch(
  () => props.game.display_name,
  () => setTimeout(fitTitle, 0),
);

function open() {
  if (missing.value) return;
  emit("open", props.game.id);
}
</script>

<template>
  <div class="card" :class="{ missing }" :title="missing ? '缺失：仓库中已找不到该游戏' : game.display_name" @click="open">
    <div class="cover">
      <img v-if="coverSrc" :src="coverSrc" alt="" @error="imgFailed = true" />
      <div v-else class="placeholder">{{ game.display_name }}</div>
      <span v-if="badge" class="badge">{{ badge }}</span>
      <span v-if="game.version_count > 1" class="version-badge">{{ game.version_count }} 版本</span>
    </div>
    <div class="card-title">
      <span ref="titleEl">{{ game.display_name }}</span>
    </div>
  </div>
</template>
