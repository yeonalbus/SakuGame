<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { api, errorText, formatBytes } from "../api";
import type { AlterRef, CandidateFile, WritebackCandidates } from "../types";

const props = defineProps<{ taskId: number; open: boolean }>();
const emit = defineEmits<{
  close: [];
  confirmed: [];
  toast: [text: string, error?: boolean];
}>();

const plan = ref<WritebackCandidates | null>(null);
const loading = ref(false);
const busy = ref(false);
const checked = ref<Record<string, boolean>>({});
const collapsed = ref<Record<string, boolean>>({});

function keyOf(scope: string, rel: string) {
  return `${scope}::${rel}`;
}

async function load() {
  if (!props.open || !props.taskId) return;
  loading.value = true;
  try {
    const p = await api.getWritebackPlan(props.taskId);
    plan.value = p;
    const next: Record<string, boolean> = {};
    for (const f of p.files) {
      // 默认勾选：required（已入库且内容变更）必选；用户路径全量默认勾；
      // 游戏本体里「未入库且自基线未变」的默认不勾（避免一确认就把整包塞进 SakuAlter）
      const unalteredBg =
        f.scope === "bg" &&
        !f.required &&
        f.reason.includes("new") &&
        !f.reason.includes("mtime");
      next[keyOf(f.scope, f.rel_path)] = !unalteredBg;
    }
    checked.value = next;
  } catch (e) {
    emit("toast", errorText(e), true);
    emit("close");
  } finally {
    loading.value = false;
  }
}

watch(
  () => [props.open, props.taskId],
  () => {
    if (props.open) void load();
    else plan.value = null;
  },
  { immediate: true },
);

interface DirGroup {
  dir: string;
  files: CandidateFile[];
}

function groupsOf(scope: "bg" | "ud"): DirGroup[] {
  const map = new Map<string, CandidateFile[]>();
  for (const f of plan.value?.files ?? []) {
    if (f.scope !== scope) continue;
    const idx = f.rel_path.lastIndexOf("/");
    const dir = idx < 0 ? "." : f.rel_path.slice(0, idx);
    if (!map.has(dir)) map.set(dir, []);
    map.get(dir)!.push(f);
  }
  return [...map.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([dir, files]) => ({ dir, files }));
}

const bgGroups = computed(() => groupsOf("bg"));
const udGroups = computed(() => groupsOf("ud"));
const hasFiles = computed(() => (plan.value?.files.length ?? 0) > 0);
const requiredCount = computed(
  () => plan.value?.files.filter((f) => f.required).length ?? 0,
);

function isChecked(f: CandidateFile) {
  if (f.required) return true;
  return checked.value[keyOf(f.scope, f.rel_path)] !== false;
}

function toggleFile(f: CandidateFile) {
  if (f.required) return;
  const k = keyOf(f.scope, f.rel_path);
  checked.value = { ...checked.value, [k]: !isChecked(f) };
}

function dirState(g: DirGroup) {
  const list = g.files.filter((f) => !f.required);
  if (list.length === 0) return { all: true, none: false, locked: true };
  const on = list.filter((f) => checked.value[keyOf(f.scope, f.rel_path)] !== false).length;
  return { all: on === list.length, none: on === 0, locked: false };
}

function toggleDir(g: DirGroup) {
  const st = dirState(g);
  const target = st.locked ? true : !st.all;
  const next = { ...checked.value };
  for (const f of g.files) {
    if (f.required) continue;
    next[keyOf(f.scope, f.rel_path)] = target;
  }
  checked.value = next;
}

function scopeState(scope: "bg" | "ud") {
  const list = (plan.value?.files ?? []).filter(
    (f) => f.scope === scope && !f.required,
  );
  const total = (plan.value?.files ?? []).filter((f) => f.scope === scope).length;
  if (total === 0) return { all: false, none: true, locked: true };
  if (list.length === 0) return { all: true, none: false, locked: true };
  const on = list.filter((f) => checked.value[keyOf(f.scope, f.rel_path)] !== false).length;
  return { all: on === list.length, none: on === 0, locked: false };
}

function toggleScope(scope: "bg" | "ud") {
  const st = scopeState(scope);
  const target = st.locked ? true : !st.all;
  const next = { ...checked.value };
  for (const f of plan.value?.files ?? []) {
    if (f.scope !== scope || f.required) continue;
    next[keyOf(f.scope, f.rel_path)] = target;
  }
  checked.value = next;
}

function toggleDirCollapse(dir: string) {
  collapsed.value = { ...collapsed.value, [dir]: !collapsed.value[dir] };
}

const selection = computed<AlterRef[]>(() => {
  const out: AlterRef[] = [];
  for (const f of plan.value?.files ?? []) {
    if (isChecked(f)) out.push({ scope: f.scope, rel_path: f.rel_path });
  }
  return out;
});

const selectedBytes = computed(() => {
  let sum = 0;
  for (const f of plan.value?.files ?? []) if (isChecked(f)) sum += f.size_bytes;
  return sum;
});

function reasonText(f: CandidateFile) {
  const parts: string[] = [];
  if (f.reason.includes("hash")) parts.push("内容变更");
  if (f.reason.includes("mtime")) parts.push("mtime 变动");
  if (parts.length === 0 && f.reason.includes("new")) parts.push("未入库");
  return parts.join(" · ") || "变动";
}

async function confirm() {
  if (!plan.value || busy.value) return;
  busy.value = true;
  try {
    await api.confirmWriteback(props.taskId, selection.value);
    emit("toast", "已确认回写");
    emit("confirmed");
    emit("close");
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busy.value = false;
  }
}

async function cancel() {
  if (busy.value) return;
  busy.value = true;
  try {
    await api.abortWriteback(props.taskId);
    emit("toast", "已中止回写，游戏仍保留在缓存池");
    emit("close");
  } catch (e) {
    emit("toast", errorText(e), true);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="wb-mask">
    <div class="wb-dialog">
      <header class="wb-head">
        <div>
          <h3>
            确认变更组文件
            <span v-if="plan" class="wb-sub">
              {{ plan.game_name }} · {{ plan.version_label }}
            </span>
          </h3>
          <p class="wb-hint">
            <template v-if="hasFiles">
              标记「自动回写」的为已入库且内容变更，必须回写；其余请勾选需要存入 SakuAlter 的文件。
            </template>
            <template v-else>没有检测到需要回写的文件，确认后将直接移除缓存副本。</template>
          </p>
        </div>
        <button class="wb-x" title="中止回写" @click="cancel">✕</button>
      </header>

      <div class="wb-body">
        <div v-if="loading" class="wb-empty">读取检测结果…</div>

        <template v-else-if="plan">
          <!-- 状态提示 -->
          <div v-if="plan.user_path_state === 'unset'" class="wb-warn">
            用户路径未配置，本次不会追踪/回写用户目录存档（可能丢失存档）
          </div>
          <div v-else-if="plan.user_path_state === 'unreachable'" class="wb-warn">
            用户路径不可达：{{ plan.user_path }}，已跳过用户目录检测
          </div>
          <div v-if="!plan.baseline_ready" class="wb-warn">
            该版本未建立 mtime 基线（v2.1 之前的旧缓存）：游戏本体内的变动无法检测，
            建议先取消并重新搬运一次（搬运会建立基线）。用户路径若已配置，本次仍会列出其全部文件。
          </div>
          <div v-if="requiredCount > 0" class="wb-note">
            已入库且内容变更的文件 {{ requiredCount }} 个，将自动回写且不可取消勾选
          </div>
          <div v-if="plan.hidden_note" class="wb-note">{{ plan.hidden_note }}</div>

          <!-- 游戏本体 -->
          <div class="wb-group">
            <label class="wb-scope">
              <input
                type="checkbox"
                :checked="scopeState('bg').all"
                :disabled="bgGroups.length === 0"
                @change="toggleScope('bg')"
              />
              <span>游戏本体变动</span>
              <span class="faint">{{ bgGroups.reduce((n, g) => n + g.files.length, 0) }} 个文件</span>
            </label>
            <div v-if="bgGroups.length === 0" class="wb-empty-line">无变动</div>
            <div v-for="g in bgGroups" :key="'bg-' + g.dir" class="wb-dir">
              <label class="wb-dir-head">
                <input
                  type="checkbox"
                  :checked="dirState(g).all"
                  @change="toggleDir(g)"
                />
                <button class="wb-dir-name" type="button" @click="toggleDirCollapse('bg-' + g.dir)">
                  {{ g.dir === "." ? "（根目录）" : g.dir }}\
                  <span class="faint">{{ collapsed['bg-' + g.dir] ? "▸" : "▾" }}</span>
                </button>
              </label>
              <div v-if="!collapsed['bg-' + g.dir]" class="wb-files">
                <label v-for="f in g.files" :key="f.rel_path" class="wb-file">
                  <input
                    type="checkbox"
                    :checked="isChecked(f)"
                    :disabled="f.required"
                    @change="toggleFile(f)"
                  />
                  <span class="wb-file-name" :class="{ req: f.required }">
                    {{ f.rel_path.split("/").pop() }}
                  </span>
                  <span class="wb-file-meta">
                    <span v-if="f.required" class="wb-req">自动回写</span>
                    <span v-else>{{ reasonText(f) }}</span>
                    <span class="faint">{{ formatBytes(f.size_bytes) }}</span>
                  </span>
                </label>
              </div>
            </div>
          </div>

          <!-- 用户路径 -->
          <div class="wb-group">
            <label class="wb-scope">
              <input
                type="checkbox"
                :checked="scopeState('ud').all"
                :disabled="udGroups.length === 0"
                @change="toggleScope('ud')"
              />
              <span>用户路径变动</span>
              <span class="faint">
                <template v-if="plan.user_path_state === 'ok'">
                  {{ udGroups.reduce((n, g) => n + g.files.length, 0) }} 个文件
                </template>
                <template v-else>未配置 / 不可达</template>
              </span>
            </label>
            <div v-if="udGroups.length === 0" class="wb-empty-line">无变动</div>
            <div v-for="g in udGroups" :key="'ud-' + g.dir" class="wb-dir">
              <label class="wb-dir-head">
                <input type="checkbox" :checked="dirState(g).all" @change="toggleDir(g)" />
                <button class="wb-dir-name" type="button" @click="toggleDirCollapse('ud-' + g.dir)">
                  {{ g.dir === "." ? "（根目录）" : g.dir }}\
                  <span class="faint">{{ collapsed['ud-' + g.dir] ? "▸" : "▾" }}</span>
                </button>
              </label>
              <div v-if="!collapsed['ud-' + g.dir]" class="wb-files">
                <label v-for="f in g.files" :key="f.rel_path" class="wb-file">
                  <input
                    type="checkbox"
                    :checked="isChecked(f)"
                    :disabled="f.required"
                    @change="toggleFile(f)"
                  />
                  <span class="wb-file-name" :class="{ req: f.required }">
                    {{ f.rel_path.split("/").pop() }}
                  </span>
                  <span class="wb-file-meta">
                    <span v-if="f.required" class="wb-req">自动回写</span>
                    <span v-else>{{ reasonText(f) }}</span>
                    <span class="faint">{{ formatBytes(f.size_bytes) }}</span>
                  </span>
                </label>
              </div>
            </div>
          </div>

          <!-- 删除项 -->
          <div v-if="plan.deleted.length > 0" class="wb-deleted">
            <div class="wb-deleted-title">相对基线已删除（不参与回写）</div>
            <div v-for="d in plan.deleted.slice(0, 20)" :key="d" class="wb-deleted-item">
              {{ d }}
            </div>
            <div v-if="plan.deleted.length > 20" class="faint">
              … 共 {{ plan.deleted.length }} 项
            </div>
          </div>
        </template>
      </div>

      <footer class="wb-foot">
        <span class="faint">
          未勾选的变动文件将随缓存清理丢弃 · 已选
          {{ selection.length }} 个 / {{ formatBytes(selectedBytes) }}
        </span>
        <div class="wb-actions">
          <button :disabled="busy" @click="cancel">取消（保持中止）</button>
          <button class="primary" :disabled="busy || !plan" @click="confirm">
            确认并开始回写
          </button>
        </div>
      </footer>
    </div>
  </div>
</template>
