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

export type ViewKey = "home" | "settings" | "detail";
