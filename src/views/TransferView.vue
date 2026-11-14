<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorText, formatBytes, onTransferProgress, onTransferState } from "../api";
import type { TransferProgressEvent, TransferStatus, TransferTaskView } from "../types";

const emit = defineEmits<{ toast: [text: string, error?: boolean]; open: [id: number] }>();

const TABS: { key: TransferStatus; label: string; dot: string }[] = [
  { key: "running", label: "搬运中", dot: "running" },
  { key: "queued", label: "排队中", dot: "queued" },
  { key: "paused", label: "暂停中", dot: "paused" },
  { key: "error", label: "出错", dot: "error" },
  { key: "done", label: "完成", dot: "done" },
];

const tasks = ref<TransferTaskView[]>([]);
const live = ref<Record<number, TransferProgressEvent>>({});
const current = ref<TransferStatus>("running");
const busy = ref<Record<number, boolean>>({});
const bulkBusy = ref(false);

let unProgress: UnlistenFn | null = null;
let unState: UnlistenFn | null = null;

const counts = computed(() => {
  const m: Record<string, number> = {};
  for (const t of tasks.value) m[t.status] = (m[t.status] ?? 0) + 1;
  return m;
});

const totalCount = computed(() => tasks.value.length);
const activeCount = computed(
  () => (counts.value.running ?? 0) + (counts.value.queued ?? 0),
);

const list = computed(() => tasks.value.filter((t) => t.status === current.value));

const currentLabel = computed(
  () => TABS.find((t) => t.key === current.value)?.label ?? "",
);

/** 批量操作随页签变化 */
const bulkActions = computed<{ key: "pause" | "resume" | "retry" | "cancel"; label: string; primary?: boolean; danger?: boolean }[]>(() => {
  switch (current.value) {
    case "running":
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
const cancel = (t: TransferTaskView) => act(t.task_id, () => api.cancelTransfer(t.task_id));
const retry = (t: TransferTaskView) => act(t.task_id, () => api.retryTransfer(t.task_id));

async function bulk(action: "pause" | "resume" | "retry" | "cancel") {
  const targets = list.value.slice();
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

      <div v-for="t in list" :key="t.task_id" class="tv-task">
        <div class="tv-task-head" @click="emit('open', t.game_id)">
          <span class="tv-task-name">{{ t.game_name }}</span>
          <span class="tv-tag">{{ t.version_label }}</span>
          <span class="tv-tag repo">{{ t.repo_no }}</span>
          <span class="tv-task-size">{{ formatBytes(t.total_bytes) }}</span>
        </div>

        <div class="tv-bar-row">
          <div class="tv-bar" :class="t.status === 'paused' ? 'paused' : t.status === 'error' ? 'error' : ''">
            <i :style="{ width: barOf(t).pct + '%' }" />
          </div>
          <span class="tv-pct">{{ barOf(t).pct.toFixed(1) }}%</span>
          <div class="tv-row-actions">
            <button v-if="t.status === 'running'" :disabled="busy[t.task_id]" @click="pause(t)">暂停</button>
            <button v-if="t.status === 'paused'" :disabled="busy[t.task_id]" @click="resume(t)">恢复</button>
            <button v-if="t.status === 'error'" :disabled="busy[t.task_id]" @click="retry(t)">重试</button>
            <button
              v-if="t.status !== 'done'"
              class="danger"
              :disabled="busy[t.task_id]"
              @click="cancel(t)"
            >
              取消
            </button>
          </div>
        </div>

        <div class="tv-meta">
          <template v-if="t.status === 'running'">
            <span>{{ formatBytes(barOf(t).written) }} / {{ formatBytes(barOf(t).total) }}</span>
            <span>{{ formatBytes(barOf(t).speed) }}/s</span>
            <span>剩余 {{ fmtEta(barOf(t).eta) }}</span>
          </template>
          <template v-else-if="t.status === 'queued'">
            <span>等待调度</span>
            <span>{{ formatBytes(t.total_bytes) }}</span>
          </template>
          <template v-else-if="t.status === 'paused'">
            <span>已暂停，可恢复续传</span>
            <span>{{ formatBytes(barOf(t).written) }} / {{ formatBytes(barOf(t).total) }}</span>
          </template>
          <template v-else-if="t.status === 'error'">
            <span>{{ formatBytes(barOf(t).written) }} / {{ formatBytes(barOf(t).total) }}</span>
            <span class="tv-err">{{ t.error || "出错" }}</span>
          </template>
          <template v-else-if="t.status === 'done'">
            <span class="tv-ok">已完成</span>
            <span>{{ formatBytes(t.total_bytes) }}</span>
          </template>
        </div>

        <div v-if="t.status === 'error' && t.failed_files.length" class="tv-failed">
          失败文件：{{ t.failed_files.slice(0, 5).join("、")
          }}{{ t.failed_files.length > 5 ? " 等" : "" }}
        </div>
      </div>
    </div>
  </div>
</template>
