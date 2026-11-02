<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorText, formatBytes, onScanFinished } from "../api";
import type { ExeCandidate, ExecutableRow, GameDetail, GameVersionRow } from "../types";

const props = defineProps<{ gameId: number }>();
const emit = defineEmits<{ back: []; toast: [text: string, error?: boolean] }>();

const detail = ref<GameDetail | null>(null);
const activeTab = ref<"local" | "versions">("local");
const imgFailed = ref(false);
const rescanning = ref(false);

// 名称 / 编号内联编辑
const editingName = ref(false);
const editingCode = ref(false);
const nameDraft = ref("");
const codeDraft = ref("");

// 启动按钮
const pickerOpen = ref(false);

// 版本列表
const expanded = ref<number[]>([]);
const nameDrafts = ref<Record<number, string>>({});
const noteDrafts = ref<Record<number, string>>({});

// 手动新建版本
const manualOpen = ref(false);
const manualDir = ref("");
const manualExes = ref<ExeCandidate[]>([]);
const manualSelected = ref<string | null>(null);
const manualBusy = ref(false);

let unlisten: UnlistenFn | null = null;

const versions = computed(() => detail.value?.versions ?? []);
const currentVersion = computed(
  () => versions.value.find((v) => v.id === detail.value?.selected_version_id) ?? null,
);
const launchLabel = computed(() =>
  currentVersion.value ? `启动：${currentVersion.value.display_name}` : "无可用版本",
);
const canLaunch = computed(() => currentVersion.value !== null);

const coverSrc = computed(() => {
  const p = detail.value?.cover_path;
  if (!p || imgFailed.value) return null;
  return convertFileSrc(p);
});

/** 类型汇总（各版本可能形态不同） */
const typeText = computed(() => {
  const vs = versions.value;
  if (vs.length === 0) return "—";
  const folders = vs.filter((v) => v.game_type === "folder").length;
  const archives = vs.length - folders;
  return [folders ? `${folders} 个文件夹` : "", archives ? `${archives} 个压缩包` : ""]
    .filter(Boolean)
    .join(" / ");
});

async function load() {
  try {
    const d = await api.getGame(props.gameId);
    detail.value = d;
    nameDraft.value = d.display_name;
    codeDraft.value = d.game_code ?? "";
    const names: Record<number, string> = {};
    const notes: Record<number, string> = {};
    for (const v of d.versions) {
      names[v.id] = v.display_name;
      notes[v.id] = v.note ?? "";
    }
    nameDrafts.value = names;
    noteDrafts.value = notes;
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

// ---- 名称 / 编号 ----

function startEditName() {
  if (!detail.value) return;
  nameDraft.value = detail.value.display_name;
  editingName.value = true;
}

async function saveName() {
  if (!detail.value || !editingName.value) return;
  const name = nameDraft.value.trim();
  if (!name) {
    emit("toast", "游戏名称不能为空", true);
    nameDraft.value = detail.value.display_name;
    editingName.value = false;
    return;
  }
  editingName.value = false;
  if (name === detail.value.display_name) return;
  try {
    await api.updateGame(detail.value.id, name, detail.value.game_code, detail.value.cover_path);
    await load();
    emit("toast", "名称已更新");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

function startEditCode() {
  if (!detail.value) return;
  codeDraft.value = detail.value.game_code ?? "";
  editingCode.value = true;
}

async function saveCode() {
  if (!detail.value || !editingCode.value) return;
  editingCode.value = false;
  const code = codeDraft.value.trim();
  const prev = detail.value.game_code ?? "";
  if (code === prev) return;
  try {
    await api.updateGame(
      detail.value.id,
      detail.value.display_name,
      code || null,
      detail.value.cover_path,
    );
    await load();
    emit("toast", "编号已更新");
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

// ---- 启动按钮（v1.1 仅占位） ----

function notImplemented() {
  emit("toast", "项目尚未实现启动功能，等待 mvp v2 完善");
}

async function selectVersion(v: GameVersionRow) {
  if (!detail.value) return;
  pickerOpen.value = false;
  if (detail.value.selected_version_id === v.id) return;
  try {
    await api.setSelectedVersion(detail.value.id, v.id);
    await load();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

// ---- 版本管理 ----

function toggle(id: number) {
  expanded.value = expanded.value.includes(id)
    ? expanded.value.filter((x) => x !== id)
    : [...expanded.value, id];
}

async function selectExe(v: GameVersionRow, e: ExecutableRow) {
  if (e.invalid || e.is_selected) return;
  try {
    await api.setSelectedExecutable(v.id, e.id);
    await load();
    emit("toast", `已将 ${e.file_name} 设为启动项`);
  } catch (err) {
    emit("toast", errorText(err), true);
  }
}

async function saveVersionName(v: GameVersionRow) {
  const name = (nameDrafts.value[v.id] ?? "").trim();
  if (!name) {
    nameDrafts.value[v.id] = v.display_name;
    return;
  }
  if (name === v.display_name) return;
  try {
    await api.updateVersion(v.id, name, noteDrafts.value[v.id]?.trim() || null);
    await load();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function saveNote(v: GameVersionRow) {
  const note = (noteDrafts.value[v.id] ?? "").trim();
  if (note === (v.note ?? "")) return;
  try {
    await api.updateVersion(v.id, nameDrafts.value[v.id] || v.display_name, note || null);
    await load();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function removeVersion(v: GameVersionRow) {
  const ok = window.confirm(
    `删除版本「${v.display_name}」？\n\n` +
      `这会丢弃该版本的可执行文件配置与备注，且重新扫描不会恢复（取消拉黑也不会）。`,
  );
  if (!ok) return;
  try {
    await api.deleteVersion(v.id);
    await load();
    emit("toast", "版本已删除并加入黑名单");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function removeGame() {
  if (!detail.value) return;
  const ok = window.confirm(
    `删除档案「${detail.value.display_name}」？\n\n` +
      `这会丢弃已填写的名称、编号、封面，以及其下所有版本的配置；` +
      `重新扫描不会恢复（取消拉黑也不会）。`,
  );
  if (!ok) return;
  try {
    await api.deleteGame(detail.value.id);
    emit("toast", "档案已删除并加入黑名单");
    emit("back");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function rescan() {
  if (!detail.value || rescanning.value) return;
  rescanning.value = true;
  try {
    await api.scanSingleGame(detail.value.id);
    emit("toast", "已开始重扫该档案");
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    rescanning.value = false;
  }
}

// ---- 手动新建版本 ----

function openManual() {
  if (!detail.value) return;
  manualOpen.value = true;
  manualDir.value = detail.value.relative_path;
  manualExes.value = [];
  manualSelected.value = null;
}

async function listExes() {
  if (!detail.value) return;
  manualBusy.value = true;
  try {
    manualExes.value = await api.listExecutableCandidates(
      detail.value.repository_id,
      manualDir.value,
      2,
    );
    if (manualExes.value.length === 0) {
      emit("toast", "该目录下没有找到可执行文件", true);
    }
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    manualBusy.value = false;
  }
}

async function confirmManual() {
  if (!detail.value || !manualSelected.value) return;
  manualBusy.value = true;
  try {
    await api.createManualVersion(detail.value.id, manualDir.value, manualSelected.value);
    manualOpen.value = false;
    await load();
    emit("toast", "版本已创建");
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    manualBusy.value = false;
  }
}

onMounted(async () => {
  await load();
  unlisten = await onScanFinished(async (s) => {
    if (s.scope === "single" && detail.value && s.repository_id === detail.value.repository_id) {
      await load();
      emit("toast", `重扫完成：新增 ${s.added}，更新 ${s.updated}，缺失 ${s.missing}`);
    }
  });
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <div v-if="!detail" class="empty">加载中…</div>
  <div v-else>
    <div class="detail-topbar">
      <button @click="emit('back')">← 返回</button>
      <div class="detail-topbar-right">
        <button :disabled="rescanning" @click="rescan">档案重扫</button>
        <button class="danger" @click="removeGame">删除档案</button>
      </div>
    </div>

    <!-- 上栏：封面 / 名称 / 编号 / 启动 -->
    <div class="detail-head">
      <div class="detail-cover">
        <img v-if="coverSrc" :src="coverSrc" alt="" @error="imgFailed = true" />
        <div v-else class="cover-placeholder">{{ detail.display_name }}</div>
        <button class="cover-edit" title="更换封面" @click="pickCover">✎</button>
      </div>

      <div class="detail-meta">
        <div class="detail-title">
          <input
            v-if="editingName"
            v-model="nameDraft"
            class="title-input"
            @keyup.enter="saveName"
            @blur="saveName"
          />
          <span v-else class="title-text">{{ detail.display_name }}</span>
          <button v-if="!editingName" class="icon-btn" title="修改名称" @click="startEditName">
            ✎
          </button>
        </div>

        <div class="detail-code">
          <span class="muted">编号</span>
          <input
            v-if="editingCode"
            v-model="codeDraft"
            class="code-input"
            placeholder="可留空"
            @keyup.enter="saveCode"
            @blur="saveCode"
          />
          <span v-else>{{ detail.game_code || "—" }}</span>
          <button v-if="!editingCode" class="icon-btn" title="修改编号" @click="startEditCode">
            ✎
          </button>
        </div>

        <div class="launch-wrap">
          <div class="launch">
            <button class="launch-main" :disabled="!canLaunch" @click="notImplemented">
              {{ launchLabel }}
            </button>
            <button
              class="launch-caret"
              :disabled="!canLaunch"
              title="选择版本"
              @click="pickerOpen = !pickerOpen"
            >
              ▾
            </button>
          </div>
          <div v-if="pickerOpen" class="launch-menu">
            <button
              v-for="v in versions"
              :key="v.id"
              class="launch-item"
              :class="{ active: v.id === detail.selected_version_id }"
              @click="selectVersion(v)"
            >
              <span>{{ v.display_name }}</span>
              <span class="faint">{{ formatBytes(v.size_bytes) }}</span>
            </button>
            <div v-if="versions.length === 0" class="faint launch-empty">无可用版本</div>
          </div>
        </div>
      </div>
    </div>

    <!-- 下栏：tab -->
    <div class="tabs">
      <button class="tab" :class="{ active: activeTab === 'local' }" @click="activeTab = 'local'">
        本机详情
      </button>
      <button
        class="tab"
        :class="{ active: activeTab === 'versions' }"
        @click="activeTab = 'versions'"
      >
        版本管理（{{ versions.length }}）
      </button>
    </div>

    <div v-if="activeTab === 'local'">
      <div class="kv">
        <dt>大小</dt>
        <dd>{{ formatBytes(detail.size_bytes) }}</dd>
        <dt>类型</dt>
        <dd>{{ typeText }}</dd>
        <dt>形态</dt>
        <dd>{{ detail.layout === "compact" ? "紧凑（单目录）" : "标准（版本目录）" }}</dd>
        <dt>状态</dt>
        <dd>{{ detail.status === "ok" ? "正常" : "缺失（仓库中已找不到）" }}</dd>
        <dt>绝对路径</dt>
        <dd>{{ detail.absolute_path || "—" }}</dd>
        <dt>相对路径</dt>
        <dd>{{ detail.relative_path }}</dd>
      </div>
    </div>

    <div v-else>
      <div class="version-list">
        <div v-for="v in versions" :key="v.id" class="version-item">
          <div class="version-row">
            <button class="icon-btn danger" title="删除版本" @click="removeVersion(v)">✕</button>
            <button class="version-toggle" @click="toggle(v.id)">
              <span class="version-name">{{ v.display_name }}</span>
              <span class="tag" v-if="v.source === 'manual'">手动</span>
              <span class="tag" v-if="v.game_type === 'archive'">压缩包</span>
              <span class="tag off" v-if="v.status === 'missing'">缺失</span>
              <span class="faint version-size">{{ formatBytes(v.size_bytes) }}</span>
              <span class="version-caret">{{ expanded.includes(v.id) ? "▾" : "▸" }}</span>
            </button>
          </div>

          <div v-if="expanded.includes(v.id)" class="version-body">
            <div class="kv">
              <dt>目录原名</dt>
              <dd>{{ v.version_label }}</dd>
              <dt>版本路径</dt>
              <dd class="mono">{{ v.relative_path }}</dd>
              <dt>版本名称</dt>
              <dd>
                <input
                  v-model="nameDrafts[v.id]"
                  class="inline-input"
                  @blur="saveVersionName(v)"
                  @keyup.enter="saveVersionName(v)"
                />
              </dd>
              <dt>备注</dt>
              <dd>
                <input
                  v-model="noteDrafts[v.id]"
                  class="inline-input"
                  placeholder="可留空"
                  @blur="saveNote(v)"
                  @keyup.enter="saveNote(v)"
                />
              </dd>
            </div>

            <div class="exe-block">
              <div class="faint exe-title">启动项（可执行文件）</div>
              <label v-for="e in v.executables" :key="e.id" class="exe-row">
                <input
                  type="radio"
                  :name="`exe-${v.id}`"
                  :checked="e.is_selected"
                  :disabled="e.invalid"
                  @change="selectExe(v, e)"
                />
                <span class="exe-name" :class="{ 'exe-invalid': e.invalid }">{{ e.file_name }}</span>
                <span class="faint mono">{{ e.relative_path }}</span>
                <span class="tag" v-if="e.source === 'manual'">手动</span>
                <span class="tag off" v-if="e.invalid">失效</span>
              </label>
              <div v-if="v.executables.length === 0" class="faint">
                未识别到可执行文件，可在下方手动新建版本。
              </div>
            </div>
          </div>
        </div>

        <div v-if="versions.length === 0" class="version-empty">
          <div>未识别到版本</div>
          <div class="faint">该目录下没有符合规范的版本结构（可执行文件或压缩包不在直接子层）</div>
        </div>
      </div>

      <button class="primary manual-open" @click="openManual">手动新建版本</button>

      <div v-if="manualOpen" class="manual-panel">
        <div class="field">
          <label>版本目录</label>
          <input v-model="manualDir" placeholder="相对仓库根，如 RJ001/Game" />
        </div>
        <div class="manual-actions">
          <button :disabled="manualBusy" @click="listExes">列出可执行文件</button>
          <span class="faint">启动项可指向版本目录下更深的位置（兜底不规范结构）</span>
        </div>
        <div class="manual-exes">
          <button
            v-for="c in manualExes"
            :key="c.relative_path"
            class="manual-exe"
            :class="{ active: manualSelected === c.relative_path }"
            @click="manualSelected = c.relative_path"
          >
            {{ c.relative_path }}
          </button>
          <div v-if="manualExes.length === 0" class="faint">暂无候选，先点击「列出可执行文件」</div>
        </div>
        <div class="manual-actions">
          <button class="primary" :disabled="!manualSelected || manualBusy" @click="confirmManual">
            确认新建
          </button>
          <button @click="manualOpen = false">取消</button>
        </div>
      </div>
    </div>
  </div>
</template>
