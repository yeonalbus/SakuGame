import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  GameDetail,
  GameRow,
  NewRepository,
  RepositoryRow,
  ScanProgress,
  ScanSummary,
} from "./types";

export const api = {
  // 仓库
  listRepositories: () => invoke<RepositoryRow[]>("list_repositories"),
  checkRepository: (repositoryId: number) =>
    invoke<RepositoryRow>("check_repository", { repositoryId }),
  addRepository: (payload: NewRepository) => invoke<number>("add_repository", { payload }),
  updateRepository: (repositoryId: number, payload: NewRepository) =>
    invoke<void>("update_repository", { repositoryId, payload }),
  deleteRepository: (repositoryId: number) =>
    invoke<void>("delete_repository", { repositoryId }),

  // 档案
  listGames: (repositoryId?: number | null) =>
    invoke<GameRow[]>("list_games", { repositoryId: repositoryId ?? null }),
  getGame: (gameId: number) => invoke<GameDetail>("get_game", { gameId }),
  updateGame: (
    gameId: number,
    displayName: string,
    gameCode: string | null,
    coverPath: string | null,
  ) => invoke<void>("update_game", { gameId, displayName, gameCode, coverPath }),

  // 扫描
  startScan: (repositoryId: number) => invoke<number>("start_scan", { repositoryId }),
  pauseScan: () => invoke<void>("pause_scan"),
  resumeScan: () => invoke<void>("resume_scan"),
  cancelScan: () => invoke<void>("cancel_scan"),
  scanStatus: () => invoke<ScanProgress | null>("scan_status"),
};

export function onScanProgress(cb: (p: ScanProgress) => void): Promise<UnlistenFn> {
  return listen<ScanProgress>("scan://progress", (e) => cb(e.payload));
}

export function onScanFinished(cb: (s: ScanSummary) => void): Promise<UnlistenFn> {
  return listen<ScanSummary>("scan://finished", (e) => cb(e.payload));
}

export function onScanFailed(
  cb: (p: { job_id: number; message: string }) => void,
): Promise<UnlistenFn> {
  return listen<{ job_id: number; message: string }>("scan://failed", (e) => cb(e.payload));
}

/** 把后端 AppError（{ kind, message }）转成可读文本 */
export function errorText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object" && "message" in err) {
    return String((err as { message: unknown }).message);
  }
  return String(err);
}

export function formatBytes(bytes: number): string {
  if (!bytes || bytes < 0) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}
