<script setup lang="ts">
import { ref, computed, onMounted } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errorText, formatBytes } from "../api";
import type { GameDetail } from "../types";

const props = defineProps<{ gameId: number }>();
const emit = defineEmits<{ back: []; toast: [text: string, error?: boolean] }>();

const detail = ref<GameDetail | null>(null);
const activeTab = ref("local");
const editing = ref(false);
const nameDraft = ref("");
const codeDraft = ref("");
const imgFailed = ref(false);

const coverSrc = computed(() => {
  const p = detail.value?.cover_path;
  if (!p || imgFailed.value) return null;
  return convertFileSrc(p);
});

async function load() {
  try {
    detail.value = await api.getGame(props.gameId);
    nameDraft.value = detail.value.display_name;
    codeDraft.value = detail.value.game_code ?? "";
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

function startEdit() {
  if (!detail.value) return;
  nameDraft.value = detail.value.display_name;
  codeDraft.value = detail.value.game_code ?? "";
  editing.value = true;
}

async function saveEdit() {
  if (!detail.value) return;
  const name = nameDraft.value.trim();
  if (!name) {
    emit("toast", "游戏名称不能为空", true);
    return;
  }
  try {
    await api.updateGame(
      detail.value.id,
      name,
      codeDraft.value.trim() || null,
      detail.value.cover_path,
    );
    editing.value = false;
    await load();
    emit("toast", "已保存");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function pickCover() {
  if (!detail.value) return;
  const picked = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"] }],
  });
  if (!picked || Array.isArray(picked)) return;
  try {
    imgFailed.value = false;
    await api.updateGame(
      detail.value.id,
      detail.value.display_name,
      detail.value.game_code,
      picked,
    );
    await load();
    emit("toast", "封面已更新");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

onMounted(load);
</script>

<template>
  <div v-if="!detail" class="empty">加载中…</div>
  <div v-else>
    <div class="section">
      <button @click="emit('back')">← 返回</button>
    </div>

    <!-- 上栏：封面 / 名称 / 大小 -->
    <div class="detail-head">
      <div class="detail-cover">
        <img v-if="coverSrc" :src="coverSrc" alt="" @error="imgFailed = true" />
        <div v-else>{{ detail.display_name }}</div>
      </div>
      <div class="detail-meta">
        <div class="detail-title">{{ detail.display_name }}</div>
        <div class="kv">
          <dt>大小</dt>
          <dd>{{ formatBytes(detail.size_bytes) }}</dd>
          <dt>游戏编号</dt>
          <dd>{{ detail.game_code || "—" }}</dd>
          <dt>类型</dt>
          <dd>{{ detail.game_type === "folder" ? "文件夹" : "压缩包" }}</dd>
          <dt>状态</dt>
          <dd>{{ detail.status === "ok" ? "正常" : "缺失（仓库中已找不到）" }}</dd>
        </div>
        <div class="section" style="margin-top: 16px; display: flex; gap: 8px">
          <button v-if="!editing" @click="startEdit">编辑信息</button>
          <button @click="pickCover">设置封面</button>
        </div>

        <div v-if="editing" class="section" style="margin-top: 16px">
          <div class="field">
            <label>名称</label>
            <input v-model="nameDraft" @keyup.enter="saveEdit" />
          </div>
          <div class="field">
            <label>编号</label>
            <input v-model="codeDraft" placeholder="可留空" @keyup.enter="saveEdit" />
          </div>
          <div style="display: flex; gap: 8px; margin-left: 80px">
            <button class="primary" @click="saveEdit">保存</button>
            <button @click="editing = false">取消</button>
          </div>
        </div>
      </div>
    </div>

    <!-- 下栏：tab -->
    <div class="tabs">
      <button class="tab" :class="{ active: activeTab === 'local' }" @click="activeTab = 'local'">
        本机详情
      </button>
    </div>

    <div v-if="activeTab === 'local'">
      <div class="kv">
        <dt>绝对路径</dt>
        <dd>{{ detail.absolute_path || "—" }}</dd>
        <dt>相对路径</dt>
        <dd>{{ detail.relative_path }}</dd>
      </div>
    </div>
  </div>
</template>
