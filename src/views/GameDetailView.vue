<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorText, formatBytes, onScanFinished, onTransferProgress, onTransferState } from "../api";
import { askConfirm } from "../confirm";
import WritebackDialog from "../components/WritebackDialog.vue";
import type {
  AlterSummary,
  ExeCandidate,
  GameDetail,
  GameVersionRow,
  TransferProgressEvent,
  VersionState,
} from "../types";

const props = defineProps<{ gameId: number }>();
const emit = defineEmits<{ back: []; toast: [text: string, error?: boolean] }>();

const detail = ref<GameDetail | null>(null);
const imgFailed = ref(false);
const rescanning = ref(false);

// 名称 / 编号内联编辑
const editingName = ref(false);
const editingCode = ref(false);
const nameDraft = ref("");
const codeDraft = ref("");

// 运行时状态（v2 搬运 / v2.1 回写）
const versionStates = ref<Record<number, VersionState>>({});
const liveProgress = ref<Record<number, TransferProgressEvent>>({});
const alterSummaries = ref<Record<number, AlterSummary>>({});
const busyVersion = ref(0);

// 版本配置抽屉（样板：右侧滑出，替代旧的内联展开）
const drawerOpen = ref(false);
const drawerVersionId = ref(0);
const drawerName = ref("");
const drawerNote = ref("");
const drawerUserPath = ref("");
const drawerExeId = ref<number | null>(null);
const drawerBusy = ref(false);

// 变更组勾选弹窗
const dialogOpen = ref(false);
const dialogTaskId = ref(0);

// 手动新建版本
const manualOpen = ref(false);
const manualDir = ref("");
const manualExes = ref<ExeCandidate[]>([]);
const manualSelected = ref<string | null>(null);
const manualBusy = ref(false);

let unlisten: UnlistenFn | null = null;
let unProgress: UnlistenFn | null = null;
let unState: UnlistenFn | null = null;

const versions = computed(() => detail.value?.versions ?? []);
const drawerVersion = computed(
  () => versions.value.find((v) => v.id === drawerVersionId.value) ?? null,
);

function hasLaunchItem(v: GameVersionRow) {
  return v.executables.some((e) => e.is_selected);
}

// ---------------------------------------------------------------------------
// 数据加载
// ---------------------------------------------------------------------------

async function load() {
  try {
    const d = await api.getGame(props.gameId);
    detail.value = d;
    nameDraft.value = d.display_name;
    codeDraft.value = d.game_code ?? "";
    await loadStates();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function loadStates() {
  try {
    const states = await api.getVersionStates(props.gameId);
    const map: Record<number, VersionState> = {};
    for (const s of states) map[s.version_id] = s;
    versionStates.value = map;
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function loadAlter(v: GameVersionRow) {
  try {
    alterSummaries.value = { ...alterSummaries.value, [v.id]: await api.getAlterSummary(v.id) };
  } catch {
    /* 摘要失败不打扰用户 */
  }
}

// ---------------------------------------------------------------------------
// 卡片状态（样板：缓存池 SSD / 仓库 HDD / 执行中）
// ---------------------------------------------------------------------------

type Position = "ssd" | "hdd" | "busy";

function taskOf(v: GameVersionRow) {
  return versionStates.value[v.id]?.task ?? null;
}

function positionOf(v: GameVersionRow): Position {
  const task = taskOf(v);
  if (task && task.status !== "done" && task.status !== "cancelled") return "busy";
  if (versionStates.value[v.id]?.cached) return "ssd";
  return "hdd";
}

/** 执行中徽章文案 */
function busyTextOf(v: GameVersionRow): string {
  const task = taskOf(v);
  if (!task) return "";
  if (task.status === "queued") return "排队中";
  if (task.status === "paused") return "已暂停";
  if (task.status === "error") return "出错";
  if (task.status === "waiting_confirm") return "待确认回写";
  const bar = progressOf(v);
  if (task.direction === "writeback") {
    switch (task.stage) {
      case "detect":
        return "检测变更组中";
      case "extract":
        return "提取变更组中";
      case "clean":
        return "移除缓存副本中";
      case "store":
        return "回写中";
    }
    return "回写中";
  }
  switch (task.stage) {
    case "inject":
      return "注入变更组中";
    case "baseline":
      return "生成初始记录中";
  }
  const pct = bar?.pct ?? 0;
  return pct >= 100 ? "收尾中" : `搬运中 ${pct.toFixed(0)}%`;
}

function progressOf(v: GameVersionRow) {
  const task = taskOf(v);
  if (!task) return null;
  const ev = liveProgress.value[task.task_id];
  const written = ev?.written_bytes ?? task.written_bytes;
  const total = ev?.total_bytes ?? task.total_bytes;
  const pct = total > 0 ? Math.min(100, (written / total) * 100) : 0;
  return { written, total, pct, speed: ev?.speed_bps ?? 0 };
}

/** 不可中断阶段（方案：注入 / 提取 / 清理 / 存储） */
function lockedStage(v: GameVersionRow) {
  const s = taskOf(v)?.stage;
  return s === "inject" || s === "baseline" || s === "extract" || s === "clean" || s === "store";
}

// ---------------------------------------------------------------------------
// 卡片主 / 次操作（样板：右侧操作区）
// ---------------------------------------------------------------------------

function primaryLabel(v: GameVersionRow): string {
  if (v.game_type === "archive") return "未就绪";
  const task = taskOf(v);
  if (task) {
    switch (task.status) {
      case "queued":
        return "排队中";
      case "running":
        return task.direction === "writeback" ? "回写中" : "搬运中";
      case "paused":
        return "继续";
      case "error":
        return "重试";
      case "waiting_confirm":
        return "待确认";
    }
  }
  if (versionStates.value[v.id]?.cached) {
    return hasLaunchItem(v) ? "启动" : "配置启动项";
  }
  return "搬运";
}

function primaryDisabled(v: GameVersionRow): boolean {
  if (v.game_type === "archive") return true;
  if (busyVersion.value === v.id) return true;
  const task = taskOf(v);
  if (!task) return false;
  // 执行中 / 排队中一律禁用主按钮（暂停、取消由旁边的次级按钮承担，与样板一致）
  if (task.status === "queued") return true;
  if (task.status === "running") return true;
  return false;
}

async function onPrimary(v: GameVersionRow) {
  if (primaryDisabled(v)) return;
  const task = taskOf(v);
  if (task) {
    if (task.status === "waiting_confirm") {
      openDialog(task.task_id);
      return;
    }
    if (task.status === "paused") return onResume(v);
    if (task.status === "error") return onRetry(v);
    return;
  }
  if (versionStates.value[v.id]?.cached) {
    if (!hasLaunchItem(v)) {
      openDrawer(v);
      return;
    }
    // 样板：每张卡片可直接启动 → 先切选中版本再启动
    busyVersion.value = v.id;
    try {
      if (detail.value && detail.value.selected_version_id !== v.id) {
        await api.setSelectedVersion(detail.value.id, v.id);
        await load();
      }
      await api.launchGame(props.gameId);
    } catch (e) {
      emit("toast", errorText(e), true);
    } finally {
      busyVersion.value = 0;
    }
    return;
  }
  // 搬运
  busyVersion.value = v.id;
  try {
    const pf = await api.preflightTransfer(v.id);
    if (!pf.ok) {
      emit("toast", preflightText(pf.reason), true);
      return;
    }
    await api.startTransfer(v.id);
    emit("toast", "已开始搬运");
    await loadStates();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busyVersion.value = 0;
  }
}

function preflightText(reason: string | null) {
  switch (reason) {
    case "pool_not_configured":
      return "缓存池未配置，请先到设置中配置";
    case "quota_exceeded":
      return "缓存池配额不足，请扩容或清理";
    case "disk_exceeded":
      return "缓存池所在磁盘空间不足";
    case "archive_unsupported":
      return "压缩包形态暂不支持搬运";
    case "already_cached":
      return "该版本已在缓存池中";
    default:
      return "无法搬运该版本";
  }
}

async function onPause(v: GameVersionRow) {
  const task = taskOf(v);
  if (!task) return;
  busyVersion.value = v.id;
  try {
    await api.pauseTransfer(task.task_id);
    await loadStates();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busyVersion.value = 0;
  }
}

async function onResume(v: GameVersionRow) {
  const task = taskOf(v);
  if (!task) return;
  busyVersion.value = v.id;
  try {
    await api.resumeTransfer(task.task_id);
    await loadStates();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busyVersion.value = 0;
  }
}

async function onRetry(v: GameVersionRow) {
  const task = taskOf(v);
  if (!task) return;
  busyVersion.value = v.id;
  try {
    await api.retryTransfer(task.task_id);
    await loadStates();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busyVersion.value = 0;
  }
}

async function onCancel(v: GameVersionRow) {
  const task = taskOf(v);
  if (!task) return;
  const ok = await askConfirm(
    "取消任务",
    `将取消「${v.display_name}」的${task.direction === "writeback" ? "回写" : "搬运"}任务。\n\n` +
      (task.direction === "writeback"
        ? "不会丢弃存档变动，游戏仍保留在缓存池中。"
        : "已复制的临时文件会被清理，不会影响仓库与缓存池。"),
    { confirmText: "取消任务", danger: true },
  );
  if (!ok) return;
  busyVersion.value = v.id;
  try {
    await api.cancelTransfer(task.task_id);
    await loadStates();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busyVersion.value = 0;
  }
}

// ---------------------------------------------------------------------------
// 移出缓存（v2.1：先检测再勾选回写）
// ---------------------------------------------------------------------------

async function removeCacheOf(v: GameVersionRow) {
  const ok = await askConfirm(
    "移出缓存",
    `将先检测「${v.display_name}」在缓存池中的变动（hash + mtime），再让你勾选需要回写到 SakuAlter 的文件，最后移除缓存副本。\n\n仓库本体不会被覆盖。`,
    { confirmText: "开始检测" },
  );
  if (!ok) return;
  try {
    await api.startWriteback(v.id);
    await loadStates();
    emit("toast", "已开始检测变更组");
    window.setTimeout(async () => {
      await loadStates();
      const t = versionStates.value[v.id]?.task;
      if (t?.status === "waiting_confirm") openDialog(t.task_id);
    }, 700);
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

function openDialog(taskId: number) {
  dialogTaskId.value = taskId;
  dialogOpen.value = true;
}

// ---------------------------------------------------------------------------
// 配置抽屉
// ---------------------------------------------------------------------------

function openDrawer(v: GameVersionRow) {
  drawerVersionId.value = v.id;
  drawerName.value = v.display_name;
  drawerNote.value = v.note ?? "";
  drawerUserPath.value = v.user_path ?? "";
  drawerExeId.value = v.executables.find((e) => e.is_selected)?.id ?? null;
  drawerOpen.value = true;
  void loadAlter(v);
}

function closeDrawer() {
  drawerOpen.value = false;
  drawerVersionId.value = 0;
}

/** 去掉游戏标识层前缀的启动项相对路径（样板的上下双行第二行） */
function exeRelPath(e: { relative_path: string }): string {
  const prefix = (detail.value?.relative_path ?? "") + "/";
  return e.relative_path.startsWith(prefix)
    ? e.relative_path.slice(prefix.length)
    : e.relative_path;
}

async function saveDrawer() {
  const v = drawerVersion.value;
  if (!v || drawerBusy.value) return;
  drawerBusy.value = true;
  try {
    const name = drawerName.value.trim() || v.display_name;
    const note = drawerNote.value.trim() || null;
    if (name !== v.display_name || note !== (v.note ?? null)) {
      await api.updateVersion(v.id, name, note);
    }
    let pathChanged = false;
    if (drawerUserPath.value.trim() !== (v.user_path ?? "")) {
      await api.setVersionUserPath(v.id, drawerUserPath.value.trim() || null);
      pathChanged = true;
    }
    if (drawerExeId.value !== null) {
      const target = v.executables.find((e) => e.id === drawerExeId.value);
      if (target && !target.is_selected) {
        await api.setSelectedExecutable(v.id, target.id);
      }
    }
    await load();
    await loadAlter(v);
    // 路径变化会触发后端重新检测：关掉旧候选弹窗，等 transfer://state 带着新候选自动再弹
    if (pathChanged && dialogOpen.value) {
      const t = versionStates.value[v.id]?.task;
      if (t && t.task_id === dialogTaskId.value) {
        dialogOpen.value = false;
        emit("toast", "用户路径已保存，正在重新检测变更组");
      }
    }
    closeDrawer();
    emit("toast", "版本配置已保存");
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    drawerBusy.value = false;
  }
}

function userPathStateText(s: AlterSummary | undefined) {
  switch (s?.user_path_state) {
    case "ok":
      return "已配置";
    case "unreachable":
      return "不可达（注入/回写会跳过）";
    default:
      return "未配置";
  }
}

function anchorStateText(s: AlterSummary | undefined) {
  switch (s?.anchor_state) {
    case "ok":
      return "正常";
    case "missing":
      return "缺失";
    case "drift":
      return "与记录不一致";
    case "none":
      return "尚未建立";
    default:
      return "—";
  }
}

// ---------------------------------------------------------------------------
// 名称 / 编号 / 封面
// ---------------------------------------------------------------------------

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
      picked as string,
    );
    await load();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

const coverSrc = computed(() => {
  const p = detail.value?.cover_path;
  if (!p || imgFailed.value) return null;
  return convertFileSrc(p);
});

// ---------------------------------------------------------------------------
// 删除 / 重扫 / 手动新建
// ---------------------------------------------------------------------------

async function removeVersion(v: GameVersionRow) {
  const ok = await askConfirm(
    "删除版本",
    `将删除版本「${v.display_name}」并加入黑名单。\n\n这会丢弃该版本的可执行文件配置与备注，且重新扫描不会恢复（取消拉黑也不会）。`,
    { confirmText: "删除", danger: true },
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
  const ok = await askConfirm(
    "删除档案",
    `将删除档案「${detail.value.display_name}」并加入黑名单。\n\n这会丢弃已填写的名称、编号、封面，以及其下所有版本的配置；重新扫描不会恢复（取消拉黑也不会）。`,
    { confirmText: "删除档案", danger: true },
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

// ---------------------------------------------------------------------------
// 生命周期
// ---------------------------------------------------------------------------

onMounted(async () => {
  await load();
  unlisten = await onScanFinished(async (s) => {
    if (s.scope === "single" && detail.value && s.repository_id === detail.value.repository_id) {
      await load();
      emit("toast", `重扫完成：新增 ${s.added}，更新 ${s.updated}，缺失 ${s.missing}`);
    }
  });
  unProgress = await onTransferProgress((p) => {
    liveProgress.value[p.task_id] = p;
    const cur = Object.values(versionStates.value).find((s) => s.task?.task_id === p.task_id);
    if (cur && cur.task && cur.task.status !== p.status) void loadStates();
  });
  unState = await onTransferState(() => {
    void loadStates().then(() => {
      if (dialogOpen.value) return;
      const mine = Object.values(versionStates.value).find(
        (s) => s.task?.status === "waiting_confirm",
      );
      if (mine?.task) openDialog(mine.task.task_id);
    });
  });
});

onUnmounted(() => {
  unlisten?.();
  unProgress?.();
  unState?.();
});
</script>

<template>
  <div v-if="!detail" class="empty">加载中…</div>
  <div v-else class="gdet">
    <!-- 1. 顶部操作导航栏 -->
    <div class="detail-topbar">
      <button class="gdet-back" @click="emit('back')">← 返回</button>
      <div class="detail-topbar-right">
        <button :disabled="rescanning" @click="rescan">档案重扫</button>
        <button class="danger" @click="removeGame">删除档案</button>
      </div>
    </div>

    <!-- 2. 游戏头部：竖版封面 + 标题 / 形态胶囊 + 编号 -->
    <section class="gdet-hero">
      <div class="gdet-cover">
        <img v-if="coverSrc" :src="coverSrc" alt="" @error="imgFailed = true" />
        <span v-else class="faint gdet-cover-code">{{ detail.game_code || detail.display_name }}</span>
        <button class="cover-edit" title="更换封面" @click="pickCover">✎</button>
      </div>

      <div class="gdet-meta">
        <div class="gdet-title-row">
          <input
            v-if="editingName"
            v-model="nameDraft"
            class="title-input"
            @keyup.enter="saveName"
            @blur="saveName"
          />
          <h1 v-else class="gdet-title">{{ detail.display_name }}</h1>
          <button v-if="!editingName" class="icon-btn" title="修改名称" @click="startEditName">✎</button>
        </div>

        <div class="gdet-sub-row">
          <span
            class="layout-pill"
            :class="detail.layout === 'compact' ? 'compact' : 'standard'"
            :title="
              detail.layout === 'compact'
                ? '紧凑形态：游戏标识层即版本层（直接含可执行文件）'
                : '标准形态：游戏标识层下含版本目录'
            "
          >
            <i />
            {{ detail.layout === "compact" ? "紧凑" : "标准" }}
          </span>
          <span class="faint">编号</span>
          <input
            v-if="editingCode"
            v-model="codeDraft"
            class="code-input"
            placeholder="可留空"
            @keyup.enter="saveCode"
            @blur="saveCode"
          />
          <span v-else class="gdet-code mono">{{ detail.game_code || "—" }}</span>
          <button v-if="!editingCode" class="icon-btn" title="修改编号" @click="startEditCode">✎</button>
        </div>
      </div>
    </section>

    <!-- 3. 单 Tab 导航 -->
    <div class="tabs">
      <button class="tab active">版本管理（{{ versions.length }}）</button>
      <span class="faint gdet-tab-hint">点卡片右侧「⚙ 配置」调整启动项与用户路径</span>
    </div>

    <!-- 4. 版本卡片列表 -->
    <section class="gdet-list">
      <div
        v-for="v in versions"
        :key="v.id"
        class="vcard"
        :class="{ busy: positionOf(v) === 'busy' }"
      >
        <div class="vcard-info">
          <div class="vcard-line1">
            <span class="vcard-name" :title="v.display_name">{{ v.display_name }}</span>
            <span v-if="positionOf(v) === 'ssd'" class="vbadge ssd"><i />缓存池 SSD</span>
            <span v-else-if="positionOf(v) === 'hdd'" class="vbadge hdd"><i />仓库 HDD</span>
            <span v-else class="vbadge busy"><i />{{ busyTextOf(v) }}</span>
            <span class="vcard-size">{{ formatBytes(v.size_bytes) }}</span>
            <span v-if="v.status === 'missing'" class="tag off">缺失</span>
            <span v-if="v.game_type === 'archive'" class="tag off">压缩包</span>
            <span v-if="v.source === 'manual'" class="tag">手动</span>
          </div>

          <div class="vcard-line2">
            <span v-if="hasLaunchItem(v)">
              启动项: <span class="mono">{{ v.executables.find((e) => e.is_selected)?.file_name }}</span>
            </span>
            <span v-else class="warn-text">未配置启动项</span>
            <span v-if="v.user_path" class="faint">用户路径: 已配置</span>
            <span class="faint mono vcard-path" :title="v.relative_path">路径: {{ v.relative_path }}</span>
            <span v-if="v.note" class="faint vcard-note" :title="v.note">{{ v.note }}</span>
          </div>

          <div v-if="positionOf(v) === 'busy'" class="vcard-progress">
            <div class="vbar">
              <i :style="{ width: (progressOf(v)?.pct ?? 0) + '%' }" />
            </div>
            <span class="faint mono">
              {{ formatBytes(progressOf(v)?.written ?? 0) }} /
              {{ formatBytes(progressOf(v)?.total ?? 0) }}
            </span>
          </div>
        </div>

        <div class="vcard-actions">
          <button
            class="vbtn primary"
            :disabled="primaryDisabled(v)"
            @click="onPrimary(v)"
          >
            {{ primaryLabel(v) }}
          </button>
          <button
            v-if="taskOf(v)?.status === 'running' && !lockedStage(v)"
            class="vbtn"
            :disabled="busyVersion === v.id"
            @click="onPause(v)"
          >
            暂停
          </button>
          <button
            v-if="taskOf(v) && taskOf(v)!.status !== 'done'"
            class="vbtn danger"
            :disabled="busyVersion === v.id"
            @click="onCancel(v)"
          >
            取消
          </button>
          <button
            v-if="versionStates[v.id]?.cached && !taskOf(v)"
            class="vbtn"
            title="检测变更组并回写后移除缓存"
            @click="removeCacheOf(v)"
          >
            移出缓存
          </button>

          <div class="vsep" />

          <button
            class="vbtn"
            :disabled="!!taskOf(v)"
            title="配置版本属性与启动项"
            @click="openDrawer(v)"
          >
            ⚙ 配置
          </button>
          <button
            class="vcard-del"
            :disabled="!!taskOf(v)"
            title="删除版本"
            @click="removeVersion(v)"
          >
            🗑
          </button>
        </div>
      </div>

      <div v-if="versions.length === 0" class="version-empty">
        <div>未识别到版本</div>
        <div class="faint">该目录下没有符合规范的版本结构（可执行文件或压缩包不在直接子层）</div>
      </div>
    </section>

    <!-- 5. 底部：手动新建版本 -->
    <div class="gdet-foot">
      <button class="primary manual-open" @click="openManual">＋ 手动新建版本</button>

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

  <!-- 6. 右侧滑出版本配置抽屉 -->
  <div v-if="drawerOpen" class="drawer-mask" @click="closeDrawer" />
  <aside class="drawer" :class="{ open: drawerOpen }">
    <div class="drawer-head">
      <div class="drawer-head-text">
        <h3>版本配置</h3>
        <p class="faint mono" :title="drawerVersion?.relative_path">
          版本路径: {{ drawerVersion?.relative_path ?? "—" }}
        </p>
      </div>
      <button class="icon-btn" title="关闭" @click="closeDrawer">✕</button>
    </div>

    <div class="drawer-body">
      <div class="drawer-field">
        <label>版本名称</label>
        <input v-model="drawerName" placeholder="版本显示名" />
      </div>

      <div class="drawer-field">
        <div class="drawer-field-head">
          <label>启动项（可执行文件）</label>
          <span class="faint">已隐藏游戏标识层前缀</span>
        </div>
        <div class="exe-list">
          <label
            v-for="e in drawerVersion?.executables ?? []"
            :key="e.id"
            class="exe-option"
            :class="{ disabled: e.invalid }"
          >
            <input
              v-model="drawerExeId"
              type="radio"
              :value="e.id"
              :disabled="e.invalid"
            />
            <span class="exe-option-text">
              <span class="exe-option-name">{{ e.file_name }}</span>
              <span class="exe-option-path" :title="exeRelPath(e)">{{ exeRelPath(e) }}</span>
            </span>
            <span v-if="e.source === 'manual'" class="tag">手动</span>
            <span v-if="e.invalid" class="tag off">失效</span>
          </label>
          <div v-if="(drawerVersion?.executables.length ?? 0) === 0" class="faint">
            未识别到可执行文件，可在下方手动新建版本。
          </div>
        </div>
      </div>

      <div class="drawer-field">
        <div class="drawer-field-head">
          <label>用户路径（绝对路径）</label>
          <span class="faint">SakuAlter/UD 追踪目录</span>
        </div>
        <input
          v-model="drawerUserPath"
          class="mono"
          placeholder="例如 C:\Users\UserName\AppData\LocalLow\Studio\Name"
        />
      </div>

      <div class="drawer-field">
        <label>备注</label>
        <textarea v-model="drawerNote" rows="2" placeholder="可留空" />
      </div>

      <div v-if="drawerVersion" class="alter-block">
        <div>
          <b>变更组</b>
          <span class="faint">
            BG {{ alterSummaries[drawerVersion.id]?.bg_count ?? 0 }} 个 ·
            UD {{ alterSummaries[drawerVersion.id]?.ud_count ?? 0 }} 个 ·
            {{ formatBytes(alterSummaries[drawerVersion.id]?.bytes ?? 0) }}
          </span>
        </div>
        <div class="faint">
          锚点：{{ anchorStateText(alterSummaries[drawerVersion.id]) }} ·
          基线：{{ alterSummaries[drawerVersion.id]?.baseline_ready ? "已建立" : "未建立（下次搬运时生成）" }} ·
          用户目录：{{ userPathStateText(alterSummaries[drawerVersion.id]) }}
        </div>
      </div>
    </div>

    <div class="drawer-foot">
      <button :disabled="drawerBusy" @click="closeDrawer">取消</button>
      <button class="primary" :disabled="drawerBusy" @click="saveDrawer">保存</button>
    </div>
  </aside>

  <!-- 7. 变更组勾选弹窗 -->
  <WritebackDialog
    :open="dialogOpen"
    :task-id="dialogTaskId"
    @close="dialogOpen = false"
    @confirmed="loadStates"
    @toast="(text, error) => emit('toast', text, error)"
  />
</template>
