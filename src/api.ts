import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  BlocklistRow,
  ExeCandidate,
  GameDetail,
  GameRow,
  NewRepository,
  RepositoryRow,
  ScanProgress,
  ScanSummary,
  UnblockOutcome,
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
  deleteGame: (gameId: number) => invoke<void>("delete_game", { gameId }),

  // 版本
  updateVersion: (versionId: number, displayName: string, note: string | null) =>
    invoke<void>("update_version", { versionId, displayName, note }),
  setSelectedVersion: (gameId: number, versionId: number) =>
    invoke<void>("set_selected_version", { gameId, versionId }),
  setSelectedExecutable: (versionId: number, executableId: number) =>
    invoke<void>("set_selected_executable", { versionId, executableId }),
  deleteVersion: (versionId: number) => invoke<void>("delete_version", { versionId }),
  createManualVersion: (
    gameId: number,
    relativePath: string,
    executableRelativePath: string,
    displayName?: string | null,
  ) =>
    invoke<number>("create_manual_version", {
      gameId,
      relativePath,
      executableRelativePath,
      displayName: displayName ?? null,
    }),
  addManualExecutable: (versionId: number, relativePath: string) =>
    invoke<number>("add_manual_executable", { versionId, relativePath }),
  listExecutableCandidates: (repositoryId: number, relativePath: string, maxDepth?: number) =>
    invoke<ExeCandidate[]>("list_executable_candidates", {
      repositoryId,
      relativePath,
      maxDepth: maxDepth ?? null,
    }),

  // 黑名单
  listBlocklist: () => invoke<BlocklistRow[]>("list_blocklist"),
  unblock: (blocklistId: number) => invoke<UnblockOutcome>("unblock", { blocklistId }),

  // 扫描
  startScan: (repositoryId: number) => invoke<number>("start_scan", { repositoryId }),
  scanSingleGame: (gameId: number) => invoke<number>("scan_single_game", { gameId }),
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
