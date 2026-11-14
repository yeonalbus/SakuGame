<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, errorText, formatBytes, onTransferProgress, onTransferState } from "../api";
import type { TransferProgressEvent, TransferStatus, TransferTaskView } from "../types";

const emit = defineEmits<{ toast: [text: string, error?: boolean]; open: [id: number] }>();

const tasks = ref<TransferTaskView[]>([]);
/** 实时进度（事件覆盖） */
const live = ref<Record<number, TransferProgressEvent>>({});
const busy = ref<Record<number, boolean>>({});
let unProgress: UnlistenFn | null = null;
let unState: UnlistenFn | null = null;

const ORDER: { key: TransferStatus; title: string }[] = [
  { key: "running", title: "搬运中" },
  { key: "queued", title: "排队中" },
  { key: "paused", title: "暂停中" },
  { key: "error", title: "出错" },
  { key: "done", title: "完成" },
];

const grouped = computed(() =>
  ORDER.map(({ key, title }) => ({
    key,
    title,
    items: tasks.value.filter((t) => t.status === key),
  })).filter((g) => g.items.length > 0),
);

function barOf(t: TransferTaskView) {
  const ev = live.value[t.task_id];
  const written = ev?.written_bytes ?? t.written_bytes;
  const total = ev?.total_bytes ?? t.total_bytes;
  const pct = total > 0 ? Math.min(100, (written / total) * 100) : 0;
  return { written, total, pct, speed: ev?.speed_bps ?? 0, eta: ev?.eta_secs ?? null };
}

function fmtEta(s: number | null) {
  if (s === null || s < 0) return "—";
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${s % 60}s`;
  return `${Math.floor(s / 3600)}h${Math.floor((s % 3600) / 60)}m`;
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
  <div class="transfer-view">
    <div class="detail-topbar">
      <h2 class="transfer-title">搬运任务</h2>
      <button @click="load">刷新</button>
    </div>

    <div v-if="tasks.length === 0" class="empty">
      暂无搬运任务<br />
      <span class="faint">在游戏详情页点「搬运：XXX」开始</span>
    </div>

    <div v-for="g in grouped" :key="g.key" class="task-group">
      <div class="task-group-title">{{ g.title }}（{{ g.items.length }}）</div>

      <div v-for="t in g.items" :key="t.task_id" class="task-card">
        <div class="task-head" @click="emit('open', t.game_id)">
          <span class="task-name">{{ t.game_name }}</span>
          <span class="tag">{{ t.version_label }}</span>
          <span class="faint">{{ t.repo_no }}</span>
          <span class="faint task-size">{{ formatBytes(t.total_bytes) }}</span>
        </div>

        <div class="task-bar-row">
          <div class="task-bar">
            <div class="task-bar-fill" :style="{ width: barOf(t).pct + '%' }" />
          </div>
          <span class="faint task-pct">{{ barOf(t).pct.toFixed(1) }}%</span>
        </div>

        <div class="task-meta">
          <span>{{ formatBytes(barOf(t).written) }} / {{ formatBytes(barOf(t).total) }}</span>
          <span v-if="t.status === 'running'" class="faint">
            {{ formatBytes(barOf(t).speed) }}/s · 剩余 {{ fmtEta(barOf(t).eta) }}
          </span>
          <span v-if="t.status === 'queued'" class="faint">等待调度</span>
          <span v-if="t.status === 'paused'" class="faint">已暂停，可恢复续传</span>
          <span v-if="t.status === 'done'" class="ok">已完成</span>
          <span v-if="t.status === 'error'" class="error-text">{{ t.error || "出错" }}</span>
        </div>

        <div v-if="t.status === 'error' && t.failed_files.length" class="faint failed-list">
          失败文件：{{ t.failed_files.slice(0, 5).join("、")
          }}{{ t.failed_files.length > 5 ? " 等" : "" }}
        </div>

        <div class="task-actions">
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
        </div>
      </div>
    </div>
  </div>
</template>
