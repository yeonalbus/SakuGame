<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorText, formatBytes, onTransferProgress, onTransferState } from "../api";
import { askConfirm } from "../confirm";
import WritebackDialog from "../components/WritebackDialog.vue";
import type { TransferProgressEvent, TransferTaskView } from "../types";

const emit = defineEmits<{ toast: [text: string, error?: boolean]; open: [id: number] }>();

/** 全局并发通道（与后端 MAX_SLOTS 对齐） */
const MAX_SLOTS = 32;

const TABS: { key: string; label: string; dot: string }[] = [
  { key: "active", label: "搬运中", dot: "running" },
  { key: "queued", label: "排队中", dot: "queued" },
  { key: "paused", label: "暂停中", dot: "paused" },
  { key: "error", label: "出错", dot: "error" },
  { key: "done", label: "完成", dot: "done" },
];

const tasks = ref<TransferTaskView[]>([]);
const live = ref<Record<number, TransferProgressEvent>>({});
const current = ref<string>("active");
const busy = ref<Record<number, boolean>>({});
const bulkBusy = ref(false);

/** 勾选弹窗 */
const dialogTaskId = ref(0);
const dialogOpen = ref(false);

let unProgress: UnlistenFn | null = null;
let unState: UnlistenFn | null = null;

/** 「搬运中」页签含 waiting_confirm（回写待确认在方案里归在搬运中） */
function bucketOf(t: TransferTaskView): string {
  if (t.status === "running" || t.status === "waiting_confirm") return "active";
  return t.status;
}

const counts = computed(() => {
  const m: Record<string, number> = {};
  for (const t of tasks.value) {
    const b = bucketOf(t);
    m[b] = (m[b] ?? 0) + 1;
  }
  return m;
});

const totalCount = computed(() => tasks.value.length);
const activeCount = computed(() => counts.value.active ?? 0);
const list = computed(() => tasks.value.filter((t) => bucketOf(t) === current.value));

/** 并发池：仅统计真正占额度的任务（waiting_confirm 不占） */
const slotsUsed = computed(() =>
  tasks.value
    .filter((t) => t.status === "running")
    .reduce((sum, t) => sum + (t.concurrency || 0), 0),
);

const currentLabel = computed(
  () => TABS.find((t) => t.key === current.value)?.label ?? "",
);

/** 批量操作随页签变化 */
const bulkActions = computed<{ key: "pause" | "resume" | "retry" | "cancel"; label: string; primary?: boolean; danger?: boolean }[]>(() => {
  switch (current.value) {
    case "active":
      return [
        { key: "pause", label: "全部暂停" },
        { key: "cancel", label: "全部取消", danger: true },
      ];
    case "queued":
      return [{ key: "cancel", label: "全部取消", danger: true }];
    case "paused":
      return [
        { key: "resume", label: "全部恢复", primary: true },
        { key: "cancel", label: "全部取消", danger: true },
      ];
    case "error":
      return [
        { key: "retry", label: "全部重试", primary: true },
        { key: "cancel", label: "全部取消", danger: true },
      ];
    default:
      return [];
  }
});

function fmtEta(s: number | null) {
  if (s === null || s < 0) return "—";
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${s % 60}s`;
  return `${Math.floor(s / 3600)}h${Math.floor((s % 3600) / 60)}m`;
}

function barOf(t: TransferTaskView) {
  const ev = live.value[t.task_id];
  const written = ev?.written_bytes ?? t.written_bytes;
  const total = ev?.total_bytes ?? t.total_bytes;
  const pct = total > 0 ? Math.min(100, (written / total) * 100) : 0;
  return { written, total, pct, speed: ev?.speed_bps ?? 0, eta: ev?.eta_secs ?? null };
}

/** 不可中断阶段（方案：提取 / 清理 / 注入 / 记录） */
function lockedStage(t: TransferTaskView) {
  return (
    t.stage === "extract" ||
    t.stage === "clean" ||
    t.stage === "store" ||
    t.stage === "inject" ||
    t.stage === "baseline"
  );
}

type StepState = "done" | "active" | "pending";

function pipelineOf(t: TransferTaskView): { key: string; label: string; state: StepState }[] {
  const def: [string, string][] =
    t.direction === "writeback"
      ? [
          ["detect", "①检测"],
          ["extract", "②提取"],
          ["clean", "③清理"],
          ["store", "④存储"],
        ]
      : [
          ["transfer", "①传输"],
          ["inject", "②注入"],
          ["baseline", "③记录"],
        ];
  let idx = def.findIndex(([k]) => k === (t.stage ?? ""));
  if (t.direction === "writeback" && t.stage === "waiting_confirm") idx = 0;
  return def.map(([key, label], i) => {
    let state: StepState = "pending";
    if (t.status === "done") state = "done";
    else if (idx >= 0) {
      if (i < idx) state = "done";
      else if (i === idx) state = "active";
    }
    return { key, label, state };
  });
}

function messageOf(t: TransferTaskView): { text: string; tone: string } {
  if (t.status === "waiting_confirm") {
    return {
      text: `检测到 ${t.total_files} 个变动文件，等待确认回写范围`,
      tone: "warn",
    };
  }
  if (t.status === "error") return { text: t.error || "出错", tone: "err" };
  if (t.status === "paused") return { text: "已暂停，可恢复续传", tone: "" };
  if (t.status === "done") {
    return {
      text: t.direction === "writeback" ? "变更组已回写" : "已搬运至缓存池",
      tone: "ok",
    };
  }
  if (t.status === "queued") return { text: "等待调度", tone: "" };
  switch (t.stage) {
    case "inject":
      return { text: "正在注入变更组文件（BG / UD）…", tone: "info" };
    case "baseline":
      return { text: "正在生成初始 mtime 记录…", tone: "info" };
    case "detect":
      return { text: "正在检测变更组（hash + mtime）…", tone: "info" };
    case "extract":
      return { text: "正在提取变更组到临时隔离区 .waiting…", tone: "info" };
    case "clean":
      return { text: "正在从缓存池安全移除游戏副本…", tone: "info" };
    case "store":
      return { text: "正在存储变更组到 SakuAlter…", tone: "info" };
    default: {
      const b = barOf(t);
      return {
        text: `${formatBytes(b.written)} / ${formatBytes(b.total)} · ${formatBytes(b.speed)}/s · 剩余 ${fmtEta(b.eta)}`,
        tone: "",
      };
    }
  }
}

async function load() {
  try {
    tasks.value = await api.listTransfers();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function act(taskId: number, fn: () => Promise<unknown>) {
  busy.value[taskId] = true;
  try {
    await fn();
    await load();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busy.value[taskId] = false;
  }
}

const pause = (t: TransferTaskView) => act(t.task_id, () => api.pauseTransfer(t.task_id));
const resume = (t: TransferTaskView) => act(t.task_id, () => api.resumeTransfer(t.task_id));
const retry = (t: TransferTaskView) => act(t.task_id, () => api.retryTransfer(t.task_id));

async function cancel(t: TransferTaskView) {
  const ok = await askConfirm(
    "取消任务",
    `将取消「${t.game_name}」的${t.direction === "writeback" ? "回写" : "搬运"}任务。\n\n` +
      (t.direction === "writeback"
        ? "不会丢弃存档变动，游戏仍保留在缓存池中。"
        : "已复制的临时文件会被清理，不会影响仓库与缓存池。"),
    { confirmText: "取消任务", danger: true },
  );
  if (!ok) return;
  await act(t.task_id, () => api.cancelTransfer(t.task_id));
}

/** waiting_confirm 的「暂缓」= 中止回写（任务停 paused，游戏与存档原样） */
async function hold(t: TransferTaskView) {
  await act(t.task_id, () => api.abortWriteback(t.task_id));
}

function openDialog(t: TransferTaskView) {
  dialogTaskId.value = t.task_id;
  dialogOpen.value = true;
}

async function bulk(action: "pause" | "resume" | "retry" | "cancel") {
  const targets = list.value.filter((t) => {
    if (action === "pause") return t.status === "running";
    if (action === "resume") return t.status === "paused";
    if (action === "retry") return t.status === "error";
    return true;
  });
  if (targets.length === 0 || bulkBusy.value) return;
  bulkBusy.value = true;
  let ok = 0;
  for (const t of targets) {
    try {
      if (action === "pause") await api.pauseTransfer(t.task_id);
      else if (action === "resume") await api.resumeTransfer(t.task_id);
      else if (action === "retry") await api.retryTransfer(t.task_id);
      else await api.cancelTransfer(t.task_id);
      ok += 1;
    } catch {
      /* 单条失败不阻塞其余 */
    }
  }
  bulkBusy.value = false;
  await load();
  emit("toast", `已处理 ${ok} / ${targets.length} 个任务`, ok !== targets.length);
}

onMounted(async () => {
  await load();
  unProgress = await onTransferProgress((p) => {
    live.value[p.task_id] = p;
    const cur = tasks.value.find((t) => t.task_id === p.task_id);
    if (cur && cur.status !== p.status) void load();
  });
  unState = await onTransferState(() => {
    void load();
  });
});

onUnmounted(() => {
  unProgress?.();
  unState?.();
});
</script>

<template>
  <div class="tv">
    <!-- 标题行 -->
    <div class="tv-head">
      <span class="tv-head-icon">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
          stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="7 10 12 15 17 10" />
          <line x1="12" y1="15" x2="12" y2="3" />
        </svg>
      </span>
      <h1>搬运任务</h1>
      <div class="tv-head-stats">
        <span>共 <b>{{ totalCount }}</b> 个任务 · 活动 <b>{{ activeCount }}</b></span>
        <span class="tv-refresh"><i class="tv-live" />实时更新</span>
      </div>
    </div>

    <!-- 状态页签 + 批量操作 -->
    <div class="tv-panel">
      <div class="tv-tabs-row">
        <div class="tv-tabs">
          <button
            v-for="t in TABS"
            :key="t.key"
            class="tv-tab"
            :class="{ active: current === t.key }"
            @click="current = t.key"
          >
            <i class="tv-dot" :class="t.dot" />
            {{ t.label }}
            <span class="tv-count">{{ counts[t.key] ?? 0 }}</span>
          </button>
        </div>
        <div class="tv-bulk">
          <button
            v-for="a in bulkActions"
            :key="a.key"
            :class="{ primary: a.primary, danger: a.danger }"
            :disabled="bulkBusy"
            @click="bulk(a.key)"
          >
            {{ a.label }}
          </button>
        </div>
      </div>
    </div>

    <!-- 并发池状态条（回运优先 · 阻塞复用暂停） -->
    <div class="tv-pool">
      <span>
        全局并发调度池：
        <b class="mono">已占 {{ slotsUsed }} / {{ MAX_SLOTS }}</b>
        <span class="faint">（回写优先于搬运 · 待确认阶段不占额度）</span>
      </span>
      <span class="faint">
        待确认任务点「选择文件」勾选回写范围
      </span>
    </div>

    <!-- 内容 -->
    <div class="tv-content">
      <div v-if="list.length === 0" class="tv-empty">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"
          stroke-linecap="round" stroke-linejoin="round">
          <path d="M3 7a2 2 0 0 1 2-2h4l2 2h6a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
        </svg>
        <div class="tv-empty-title">「{{ currentLabel }}」下暂无任务</div>
        <div class="tv-empty-sub">切换上方页签查看其它状态。</div>
      </div>

      <div
        v-for="t in list"
        :key="t.task_id"
        class="tv-task"
        :class="{ wc: t.status === 'waiting_confirm' }"
      >
        <div class="tv-task-head" @click="emit('open', t.game_id)">
          <span
            class="tv-dir"
            :class="t.direction === 'writeback' ? 'up' : 'down'"
          >
            {{ t.direction === "writeback" ? "↑ 回写" : "↓ 搬运" }}
          </span>
          <span class="tv-task-name">{{ t.game_name }}</span>
          <span class="tv-tag">{{ t.version_label }}</span>
          <span class="tv-tag repo">{{ t.repo_no }}</span>
          <span v-if="t.concurrency > 0 && t.status === 'running'" class="tv-conc mono">
            {{ t.total_files.toLocaleString() }} 文件 · 占 {{ t.concurrency }} 并发
          </span>
          <span class="tv-task-size">{{ formatBytes(t.total_bytes) }}</span>
        </div>

        <div class="tv-bar-row">
          <div
            class="tv-bar"
            :class="t.status === 'paused' ? 'paused' : t.status === 'error' ? 'error' : ''"
          >
            <i :style="{ width: barOf(t).pct + '%' }" />
          </div>
          <span class="tv-pct">
            {{ t.status === "waiting_confirm" ? "待确认" : barOf(t).pct.toFixed(1) + "%" }}
          </span>
          <div class="tv-row-actions">
            <template v-if="t.status === 'waiting_confirm'">
              <button :disabled="busy[t.task_id]" @click="openDialog(t)">选择文件</button>
              <button :disabled="busy[t.task_id]" @click="hold(t)">暂缓</button>
              <button class="danger" :disabled="busy[t.task_id]" @click="cancel(t)">取消</button>
            </template>
            <template v-else-if="lockedStage(t) && t.status === 'running'">
              <button disabled>处理中...</button>
            </template>
            <template v-else>
              <button
                v-if="t.status === 'running'"
                :disabled="busy[t.task_id]"
                @click="pause(t)"
              >
                暂停
              </button>
              <button
                v-if="t.status === 'paused'"
                :disabled="busy[t.task_id]"
                @click="resume(t)"
              >
                恢复
              </button>
              <button
                v-if="t.status === 'error'"
                :disabled="busy[t.task_id]"
                @click="retry(t)"
              >
                重试
              </button>
              <button
                v-if="t.status !== 'done'"
                class="danger"
                :disabled="busy[t.task_id]"
                @click="cancel(t)"
              >
                取消
              </button>
            </template>
          </div>
        </div>

        <div class="tv-meta">
          <span :class="messageOf(t).tone">{{ messageOf(t).text }}</span>
        </div>

        <div class="tv-steps">
          <span
            v-for="(s, i) in pipelineOf(t)"
            :key="s.key"
            class="tv-step"
            :class="s.state"
          >
            {{ s.label }}
            <span v-if="i < pipelineOf(t).length - 1" class="tv-step-arrow">→</span>
          </span>
        </div>

        <div v-if="t.status === 'error' && t.failed_files.length" class="tv-failed">
          失败文件：{{ t.failed_files.slice(0, 5).join("、") }}{{ t.failed_files.length > 5 ? " 等" : "" }}
        </div>
      </div>
    </div>

    <WritebackDialog
      :open="dialogOpen"
      :task-id="dialogTaskId"
      @close="dialogOpen = false"
      @confirmed="load"
      @toast="(text, error) => emit('toast', text, error)"
    />
  </div>
</template>
