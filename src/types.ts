// 与 Rust 侧 models.rs 对应的类型（字段保持 snake_case，与 serde 输出一致）

export type GameType = "folder" | "archive";
export type GameStatus = "ok" | "missing";
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

export interface GameRow {
  id: number;
  repository_id: number;
  relative_path: string;
  group_key: string;
  game_type: GameType;
  display_name: string;
  game_code: string | null;
  cover_path: string | null;
  size_bytes: number;
  status: GameStatus;
  updated_at: string;
}

export interface GameDetail extends GameRow {
  absolute_path: string | null;
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
}

export interface ScanSummary {
  job_id: number;
  repository_id: number;
  found_count: number;
  added: number;
  updated: number;
  missing: number;
  warning_count: number;
}

export interface AppErrorPayload {
  kind: string;
  message: string;
}

export type ViewKey = "home" | "settings" | "detail";
