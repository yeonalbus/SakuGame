// 与 Rust 侧 models.rs 对应的类型（字段保持 snake_case，与 serde 输出一致）

export type GameType = "folder" | "archive";
/** v1.1：紧凑（游戏标识层自带可执行文件/压缩包）｜标准（由版本层组织） */
export type GameLayout = "compact" | "standard";
export type GameStatus = "ok" | "missing";
export type VersionSource = "scan" | "manual";
export type ScanScope = "full" | "single";
export type LocatorKind = "drive" | "ip" | "host";
export type ScanPhase = "idle" | "scanning" | "paused" | "finishing";

export interface RepositoryLocator {
  kind: LocatorKind | string;
  path_template: string;
  last_ok_at: string | null;
}

export interface RepositoryRow {
  id: number;
  name: string;
  share_name: string | null;
  relative_root: string;
  active_locator: string | null;
  is_offline: boolean;
  last_scan_at: string | null;
  locators: RepositoryLocator[];
  resolved_path: string | null;
}

export interface NewLocator {
  kind: LocatorKind | string;
  path_template: string;
}

export interface NewRepository {
  name?: string | null;
  share_name?: string | null;
  relative_root: string;
  locators: NewLocator[];
}

/** 档案行（= 游戏标识层，1 游戏 1 行） */
export interface GameRow {
  id: number;
  repository_id: number;
  relative_path: string;
  layout: GameLayout;
  display_name: string;
  game_code: string | null;
  cover_path: string | null;
  size_bytes: number;
  status: GameStatus;
  version_count: number;
  selected_version_id: number | null;
  selected_version_label: string | null;
  updated_at: string;
}

/** 版本内的可执行文件 */
export interface ExecutableRow {
  id: number;
  relative_path: string;
  file_name: string;
  size_bytes: number;
  is_selected: boolean;
  source: VersionSource;
  /** 上次重扫已找不到该文件 */
  invalid: boolean;
}

/** 版本行 */
export interface GameVersionRow {
  id: number;
  game_id: number;
  relative_path: string;
  version_label: string;
  display_name: string;
  note: string | null;
  game_type: GameType;
  size_bytes: number;
  source: VersionSource;
  status: GameStatus;
  /** v2.1：用户目录绝对路径；null = 未配置 */
  user_path: string | null;
  executables: ExecutableRow[];
  updated_at: string;
}

export interface GameDetail extends GameRow {
  versions: GameVersionRow[];
  absolute_path: string | null;
}

export interface BlocklistRow {
  id: number;
  repository_id: number;
  kind: "game" | "version";
  relative_path: string;
  created_at: string;
}

/** 取消拉黑的结果：restored = 是否已立即恢复到列表 / 主页 */
export interface UnblockOutcome {
  restored: boolean;
  message: string;
}

/** 手动配置时的可执行文件候选 */
export interface ExeCandidate {
  relative_path: string;
  file_name: string;
  size_bytes: number;
}

export interface ScanProgress {
  job_id: number;
  repository_id: number;
  done: number;
  total: number;
  found_games: number;
  current: string | null;
  phase: ScanPhase;
  warnings: number;
  scope: ScanScope;
}

export interface ScanSummary {
  job_id: number;
  repository_id: number;
  found_count: number;
  added: number;
  updated: number;
  missing: number;
  warning_count: number;
  root_loose_files: number;
  truncated_with_version_dirs: number;
  enum_failed: number;
  scope: ScanScope;
}

export interface AppErrorPayload {
  kind: string;
  message: string;
}

// ---- v2：缓存池 / 搬运 ----

/** v2.1：waiting_confirm = 回写检测完成、等待勾选，不占并发额度 */
export type TransferStatus =
  | "queued"
  | "running"
  | "paused"
  | "waiting_confirm"
  | "error"
  | "done";

export type PreflightReason =
  | "pool_not_configured"
  | "invalid_version"
  | "archive_unsupported"
  | "already_cached"
  | "quota_exceeded"
  | "disk_exceeded";

export interface CacheConfig {
  /** 绝对路径；null = 未配置 */
  root: string | null;
  quota_bytes: number;
}

export interface CacheUsage {
  quota_bytes: number;
  used_bytes: number;
  free_bytes: number;
  disk_total_bytes: number;
  disk_free_bytes: number;
  safety_bytes: number;
  available_bytes: number;
}

export interface TransferPreflight {
  ok: boolean;
  reason: PreflightReason | null;
  required_bytes: number;
  available_quota_bytes: number;
  available_disk_bytes: number;
}

export interface TransferTaskView {
  task_id: number;
  repository_id: number;
  game_id: number;
  version_id: number;
  repo_no: string;
  game_name: string;
  version_label: string;
  /** pull = 仓库→池；writeback = 池→仓库 SakuAlter */
  direction: "pull" | "writeback" | string;
  /** v2.1 阶段键：transfer / inject / baseline / detect / waiting_confirm / extract / clean / store */
  stage: string | null;
  /** 该任务占用的并发额度 */
  concurrency: number;
  total_bytes: number;
  written_bytes: number;
  total_files: number;
  written_files: number;
  status: TransferStatus;
  speed_bps: number;
  eta_secs: number | null;
  error: string | null;
  failed_files: string[];
}

export interface TransferProgressEvent {
  task_id: number;
  written_bytes: number;
  total_bytes: number;
  speed_bps: number;
  eta_secs: number | null;
  status: TransferStatus;
}

/** 详情页按钮态机用的版本运行时状态 */
export interface VersionState {
  version_id: number;
  cached: boolean;
  cache_missing: boolean;
  task: TransferTaskView | null;
}

export interface RepositoryDeletePreview {
  cache_count: number;
  cache_bytes: number;
  active_task_count: number;
}

// ---- v2.1：变更组 / 锚点 / 回写 ----

/** 变更组文件引用（勾选确认 / 回写选择） */
export interface AlterRef {
  /** bg = 游戏本体；ud = 用户路径 */
  scope: "bg" | "ud" | string;
  rel_path: string;
}

/** 勾选树叶子 */
export interface CandidateFile {
  scope: "bg" | "ud" | string;
  /** bg：相对版本层根；ud：相对 user_path */
  rel_path: string;
  size_bytes: number;
  mtime_ms: number;
  /** hash / mtime（可同时命中） */
  reason: string[];
  /** true = 已有变更组文件 hash 失配，必须回写且不可取消勾选 */
  required: boolean;
}

/** 移出缓存前的候选集合 */
export interface WritebackCandidates {
  version_id: number;
  game_name: string;
  version_label: string;
  files: CandidateFile[];
  /** 相对基线被删除的路径（仅展示） */
  deleted: string[];
  user_path: string | null;
  user_path_state: "ok" | "unset" | "unreachable" | string;
  /** false = 无基线（v2.1 之前的旧缓存），mtime 部分跳过 */
  baseline_ready: boolean;
  /** false = 无变更组记录，hash 部分跳过 */
  alter_ready: boolean;
}

/** 变更组摘要（详情页配置抽屉） */
export interface AlterSummary {
  version_id: number;
  bg_count: number;
  ud_count: number;
  bytes: number;
  /** ok / missing / drift / none */
  anchor_state: string;
  baseline_ready: boolean;
  user_path_state: string;
  user_path: string | null;
}

export type ViewKey = "home" | "transfer" | "settings" | "detail";
