<script setup lang="ts">
import { ref, reactive, computed, onMounted, onUnmounted } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { api, errorText, formatBytes, onScanFailed, onScanFinished, onScanProgress } from "../api";
import { askConfirm } from "../confirm";
import type {
  BlocklistRow,
  CacheUsage,
  NewRepository,
  RepositoryRow,
  ScanProgress,
} from "../types";

const emit = defineEmits<{ toast: [text: string, error?: boolean] }>();

const tab = ref<"repos" | "blocklist" | "about">("repos");
const repos = ref<RepositoryRow[]>([]);
const blocklist = ref<BlocklistRow[]>([]);
const progress = ref<ScanProgress | null>(null);
const version = ref("");
const showForm = ref(false);
const busy = ref(false);

/** 最近一次扫描的可见性告警明细 */
const lastWarnings = ref<{
  root_loose_files: number;
  truncated_with_version_dirs: number;
  enum_failed: number;
} | null>(null);

const form = reactive({ drive: "", ip: "", host: "" });
const unlisteners: UnlistenFn[] = [];

// ---- v2：缓存池 ----
const cacheRoot = ref("");
const cacheQuotaValue = ref(10);
const cacheUnit = ref<"MB" | "GB">("GB");
const cacheUsage = ref<CacheUsage | null>(null);
/** 有缓存或在途任务时只锁「路径」；容量随时可改（受缩容/扩容校验约束） */
const cachePathLocked = ref(false);

const cacheQuotaBytes = computed(() =>
  cacheUnit.value === "GB"
    ? Math.round(cacheQuotaValue.value * 1024 * 1024 * 1024)
    : Math.round(cacheQuotaValue.value * 1024 * 1024),
);
const cachePct = computed(() => {
  const u = cacheUsage.value;
  if (!u || u.quota_bytes <= 0) return 0;
  return Math.min(100, (u.used_bytes / u.quota_bytes) * 100);
});

/** 可设容量范围：下限 = 已用（缩容），上限 = 已用 + 物理可用（扩容） */
const quotaRange = computed(() => {
  const u = cacheUsage.value;
  if (!u || !u.disk_total_bytes) return null;
  return {
    min: u.used_bytes,
    max: u.used_bytes + Math.max(0, u.available_bytes),
  };
});

const quotaInvalid = computed(() => {
  const r = quotaRange.value;
  if (!r) return false;
  const q = cacheQuotaBytes.value;
  return q < r.min || q > r.max;
});

async function loadCache() {
  try {
    const [cfg, usage] = await api.getCacheConfig();
    cacheRoot.value = cfg.root ?? "";
    cacheUsage.value = usage;
    const gb = cfg.quota_bytes / 1024 / 1024 / 1024;
    if (Number.isInteger(gb) && gb >= 1) {
      cacheUnit.value = "GB";
      cacheQuotaValue.value = gb;
    } else {
      cacheUnit.value = "MB";
      cacheQuotaValue.value = Math.round(cfg.quota_bytes / 1024 / 1024);
    }
    // 有缓存或有在途任务时锁定路径（后端仍会二次校验）
    cachePathLocked.value = usage.used_bytes > 0;
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function saveCache() {
  const root = cacheRoot.value.trim() || null;
  const quota = cacheQuotaBytes.value;
  if (!Number.isFinite(quota) || quota <= 0) {
    emit("toast", "容量必须为正整数", true);
    return;
  }
  try {
    await api.setCacheConfig(root, quota);
    await loadCache();
    emit("toast", "缓存池配置已保存");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

const scanning = computed(
  () => progress.value?.phase === "scanning" || progress.value?.phase === "paused",
);
const paused = computed(() => progress.value?.phase === "paused");
const pct = computed(() => {
  const p = progress.value;
  if (!p || !p.total) return 0;
  return Math.min(100, Math.round((p.done / p.total) * 100));
});
const warningTotal = computed(() => {
  const w = lastWarnings.value;
  if (!w) return 0;
  return w.root_loose_files + w.truncated_with_version_dirs + w.enum_failed;
});

/** 黑名单收纳层：以档案为顶层折叠显示（"RJ123/" 仅作视觉归纳，不承担功能） */
interface BlockGroup {
  key: string;
  firstAt: string;
  items: BlocklistRow[];
}

const blockGroups = computed<BlockGroup[]>(() => {
  const map = new Map<string, BlockGroup>();
  for (const row of blocklist.value) {
    const key =
      row.kind === "game" ? row.relative_path : (row.relative_path.split("/")[0] ?? row.relative_path);
    let group = map.get(key);
    if (!group) {
      group = { key, firstAt: row.created_at, items: [] };
      map.set(key, group);
    } else if (row.created_at < group.firstAt) {
      group.firstAt = row.created_at;
    }
    group.items.push(row);
  }
  const groups = [...map.values()];
  for (const group of groups) {
    // 平铺层：档案在前、版本在后；同类按路径排序
    group.items.sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "game" ? -1 : 1;
      return a.relative_path.localeCompare(b.relative_path);
    });
  }
  // 收纳层：按拉黑顺序（最早的在前）
  groups.sort((a, b) => a.firstAt.localeCompare(b.firstAt));
  return groups;
});

function shortVersionName(path: string): string {
  const parts = path.split("/");
  return parts.length > 1 ? parts[parts.length - 1] : path;
}

async function loadRepos() {
  try {
    repos.value = await api.listRepositories();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function loadBlocklist() {
  try {
    blocklist.value = await api.listBlocklist();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function refreshProgress() {
  try {
    progress.value = await api.scanStatus();
  } catch {
    /* 忽略 */
  }
}

async function scan(repo: RepositoryRow) {
  try {
    busy.value = true;
    lastWarnings.value = null;
    await api.startScan(repo.id);
    await refreshProgress();
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busy.value = false;
  }
}

async function pause() {
  try {
    await api.pauseScan();
    await refreshProgress();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function resume() {
  try {
    await api.resumeScan();
    await refreshProgress();
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function cancel() {
  try {
    await api.cancelScan();
    progress.value = null;
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function unblock(row: BlocklistRow) {
  try {
    const outcome = await api.unblock(row.id);
    await loadBlocklist();
    emit("toast", outcome.message, false);
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

function lastSegment(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const idx = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
}

async function addRepo() {
  const drive = form.drive.trim();
  if (!drive) {
    emit("toast", "盘符路径为必填项", true);
    return;
  }
  const locators: NewRepository["locators"] = [{ kind: "drive", path_template: drive }];
  if (form.ip.trim()) locators.push({ kind: "ip", path_template: form.ip.trim() });
  if (form.host.trim()) locators.push({ kind: "host", path_template: form.host.trim() });

  const payload: NewRepository = {
    relative_root: lastSegment(drive),
    locators,
  };
  try {
    await api.addRepository(payload);
    form.drive = "";
    form.ip = "";
    form.host = "";
    showForm.value = false;
    await loadRepos();
    emit("toast", "仓库已添加");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function removeRepo(repo: RepositoryRow) {
  let extra = "";
  let blocked = false;
  try {
    const pv = await api.getRepositoryDeletePreview(repo.id);
    if (pv.active_task_count > 0) {
      emit("toast", "该仓库还有搬运任务，请先取消任务再删除", true);
      blocked = true;
    } else if (pv.cache_count > 0) {
      extra =
        `\n\n该仓库有 ${pv.cache_count} 个游戏在缓存池中（占用 ${formatBytes(pv.cache_bytes)}），` +
        `删除仓库会一并清理这些池内文件。`;
    }
  } catch {
    /* 预览失败不阻塞删除流程 */
  }
  if (blocked) return;
  const ok = await askConfirm(
    "删除仓库",
    `将删除仓库「${repo.name}」。\n\n` +
      `该仓库下所有档案与版本都会被一并删除（含手填的名称、编号、封面），` +
      `且不进入黑名单；重新添加后需要重新扫描。` +
      extra,
    { confirmText: "删除仓库", danger: true },
  );
  if (!ok) return;
  try {
    await api.deleteRepository(repo.id);
    await loadRepos();
    await loadBlocklist();
    await loadCache();
    emit("toast", "仓库已删除");
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

async function checkRepo(repo: RepositoryRow) {
  try {
    const updated = await api.checkRepository(repo.id);
    await loadRepos();
    emit(
      "toast",
      updated.is_offline
        ? "仓库不可达，请检查网络配置"
        : `可达：${updated.resolved_path ?? updated.name}`,
      updated.is_offline,
    );
  } catch (e) {
    emit("toast", errorText(e), true);
  }
}

function fmtTime(v: string | null): string {
  if (!v) return "—";
  const d = new Date(v);
  if (Number.isNaN(d.getTime())) return v;
  return d.toLocaleString();
}

onMounted(async () => {
  await loadRepos();
  await loadBlocklist();
  await loadCache();
  await refreshProgress();
  version.value = await getVersion().catch(() => "0.1.0");
  unlisteners.push(
    await onScanProgress((p) => {
      progress.value = p;
    }),
  );
  unlisteners.push(
    await onScanFinished((s) => {
      progress.value = null;
      lastWarnings.value = {
        root_loose_files: s.root_loose_files,
        truncated_with_version_dirs: s.truncated_with_version_dirs,
        enum_failed: s.enum_failed,
      };
      void loadRepos();
      const scopeText = s.scope === "single" ? "（单档案）" : "";
      emit(
        "toast",
        `扫描完成${scopeText}：新增 ${s.added}，更新 ${s.updated}，缺失 ${s.missing}，警告 ${s.warning_count}`,
        false,
      );
    }),
  );
  unlisteners.push(
    await onScanFailed((p) => {
      progress.value = null;
      emit("toast", p.message, true);
    }),
  );
});

onUnmounted(() => {
  unlisteners.forEach((fn) => fn());
});
</script>

<template>
  <div class="settings-layout">
    <nav class="sub-nav">
      <button class="nav-item" :class="{ active: tab === 'repos' }" @click="tab = 'repos'">
        仓库管理
      </button>
      <button
        class="nav-item"
        :class="{ active: tab === 'blocklist' }"
        @click="
          tab = 'blocklist';
          loadBlocklist();
        "
      >
        黑名单（{{ blocklist.length }}）
      </button>
      <button class="nav-item" :class="{ active: tab === 'about' }" @click="tab = 'about'">
        关于
      </button>
    </nav>

    <div v-if="tab === 'repos'">
      <!-- v2：缓存池 -->
      <div class="section cache-section">
        <h3>缓存管理</h3>
        <div class="cache-row">
          <div class="cache-ring">
            <svg viewBox="0 0 36 36">
              <circle cx="18" cy="18" r="15.915" class="ring-bg" />
              <circle
                cx="18"
                cy="18"
                r="15.915"
                class="ring-fg"
                :style="{ strokeDasharray: cachePct + ' 100' }"
              />
            </svg>
            <div class="ring-text">{{ cachePct.toFixed(0) }}%</div>
          </div>
          <div class="cache-info">
            <div class="cache-line">
              已用 {{ formatBytes(cacheUsage?.used_bytes ?? 0) }} /
              {{ formatBytes(cacheUsage?.quota_bytes ?? 0) }}
            </div>
            <div class="faint">剩余配额 {{ formatBytes(cacheUsage?.free_bytes ?? 0) }}</div>
            <div class="faint">
              盘剩余 {{ formatBytes(cacheUsage?.disk_free_bytes ?? 0) }} · 安全线
              {{ formatBytes(cacheUsage?.safety_bytes ?? 0) }} · 可用
              {{ formatBytes(cacheUsage?.available_bytes ?? 0) }}
            </div>
          </div>
        </div>

        <div class="field">
          <label>缓存池路径</label>
          <input
            v-model="cacheRoot"
            :disabled="cachePathLocked"
            placeholder="例如 D:\SakuCache"
          />
          <div v-if="cachePathLocked" class="faint">
            已有缓存或搬运任务，路径已锁定；请先清空缓存池再修改
          </div>
        </div>

        <div class="field">
          <label>容量</label>
          <div class="quota-row">
            <input
              v-model.number="cacheQuotaValue"
              type="number"
              min="1"
              step="1"
            />
            <div class="unit-toggle">
              <button :class="{ active: cacheUnit === 'GB' }" @click="cacheUnit = 'GB'">GB</button>
              <button :class="{ active: cacheUnit === 'MB' }" @click="cacheUnit = 'MB'">MB</button>
            </div>
          </div>
          <div class="faint">
            默认 10 GB；缩容不低于已用，扩容后盘上至少留出盘总容量 10% 的余量
          </div>
          <div class="faint" v-if="quotaRange">
            可设范围：{{ formatBytes(quotaRange.min) }} ~ {{ formatBytes(quotaRange.max) }}
            <span v-if="quotaInvalid" class="error-text">（当前输入超出范围）</span>
          </div>
        </div>

        <button class="primary" :disabled="quotaInvalid" @click="saveCache">
          保存缓存池配置
        </button>
      </div>

      <!-- 上栏：进度区 -->
      <div class="progress-area">
        <div v-if="progress">
          <div class="progress-row">
            <span>{{ progress.done }} / {{ progress.total }}</span>
            <span class="muted">
              {{ progress.scope === "single" ? "单档案重扫" : "已发现 " + progress.found_games + " 个" }}
            </span>
          </div>
          <div class="progress-bar"><i :style="{ width: pct + '%' }"></i></div>
          <div class="faint" style="margin-top: 6px">
            {{ progress.current || "准备中…" }}
          </div>
          <div style="display: flex; gap: 8px; margin-top: 12px">
            <button v-if="!paused" @click="pause">暂停</button>
            <button v-else @click="resume">继续</button>
            <button class="danger" @click="cancel">取消</button>
          </div>
        </div>
        <div v-else class="muted">当前没有正在进行的扫描</div>
      </div>

      <!-- 可见性告警明细 -->
      <div v-if="lastWarnings && warningTotal > 0" class="warn-box">
        <div class="warn-title">上次扫描告警（{{ warningTotal }}）</div>
        <div v-if="lastWarnings.root_loose_files" class="warn-line">
          · 仓库根目录下有 {{ lastWarnings.root_loose_files }} 个散落文件未被识别（请放入子目录）
        </div>
        <div v-if="lastWarnings.truncated_with_version_dirs" class="warn-line">
          · {{ lastWarnings.truncated_with_version_dirs }} 个目录因自身含可执行文件被截断，其子目录未参与版本识别
        </div>
        <div v-if="lastWarnings.enum_failed" class="warn-line">
          · {{ lastWarnings.enum_failed }} 个目录枚举失败（权限 / 长路径 / 网络）
        </div>
      </div>

      <!-- 下栏：扫描按钮 + 路径条目 -->
      <div class="section">
        <h3>仓库路径</h3>

        <div v-for="r in repos" :key="r.id" class="repo-item">
          <div class="repo-head">
            <div>
              <span class="tag" :class="r.is_offline ? 'off' : 'ok'">
                {{ r.is_offline ? "离线" : "已配置" }}
              </span>
              <span class="muted" style="margin-left: 8px">{{ r.name }}</span>
            </div>
            <div style="display: flex; gap: 6px">
              <button :disabled="scanning || busy" @click="scan(r)">扫描</button>
              <button @click="checkRepo(r)">检测</button>
              <button class="danger" :disabled="scanning" @click="removeRepo(r)">删除</button>
            </div>
          </div>
          <div class="repo-path">{{ r.locators.map((l) => l.path_template).join("  |  ") }}</div>
          <div class="faint" style="margin-top: 4px">
            上次扫描：{{ fmtTime(r.last_scan_at) }}
          </div>
        </div>

        <div v-if="repos.length === 0" class="faint" style="padding: 12px 0">
          还没有配置仓库路径
        </div>

        <button class="primary" style="margin-top: 6px" @click="showForm = !showForm">
          {{ showForm ? "收起" : "添加路径" }}
        </button>

        <div v-if="showForm" class="repo-item" style="margin-top: 12px">
          <div class="field">
            <label>盘符路径 *</label>
            <input v-model="form.drive" placeholder="例如 Z:\Game" />
          </div>
          <div class="field">
            <label>IP 路径</label>
            <input v-model="form.ip" placeholder="可选，例如 \\192.168.1.10\资源盘\Game" />
          </div>
          <div class="field">
            <label>主机名路径</label>
            <input v-model="form.host" placeholder="可选，例如 \\nas\资源盘\Game" />
          </div>
          <div class="faint" style="margin: 4px 0 12px">
            盘符为必填；检测与扫描时按 盘符 → IP → 主机名 的可达性依次尝试。
          </div>
          <button class="primary" @click="addRepo">确认添加</button>
        </div>
      </div>
    </div>

    <div v-else-if="tab === 'blocklist'">
      <div class="section">
        <h3>黑名单</h3>
        <div class="faint" style="margin-bottom: 12px; line-height: 1.8">
          删除的档案与版本会保留在这里，避免下次扫描重复判定。<br />
          取消拉黑会<strong>立即恢复</strong>（档案 → 主页；版本 → 所属档案），不必等下一次扫描。<br />
          但已丢弃的手填数据（名称 / 编号 / 封面 / 版本配置）不会恢复。
        </div>

        <div v-for="g in blockGroups" :key="g.key" class="block-group">
          <div class="block-group-head">
            <span class="block-group-name">{{ g.key }}/</span>
            <span class="faint">{{ g.items.length }} 条</span>
          </div>
          <div v-for="row in g.items" :key="row.id" class="block-row">
            <span class="tag" :class="row.kind === 'game' ? 'ok' : ''">
              {{ row.kind === "game" ? "档案" : "版本" }}
            </span>
            <span class="mono block-path" :title="row.relative_path">
              {{ row.kind === "game" ? row.relative_path : shortVersionName(row.relative_path) }}
            </span>
            <button @click="unblock(row)">取消拉黑</button>
          </div>
        </div>

        <div v-if="blocklist.length === 0" class="faint">黑名单为空</div>
      </div>
    </div>

    <div v-else>
      <div class="section">
        <h3>关于</h3>
        <div class="kv">
          <dt>版本号</dt>
          <dd>{{ version }}</dd>
          <dt>构筑版本号</dt>
          <dd>{{ version }}</dd>
        </div>
      </div>
    </div>
  </div>
</template>
