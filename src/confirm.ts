// 应用内确认对话框（替代 window.confirm：系统弹窗样式不可控，且在 WebView 下行为不一致）
import { ref } from "vue";

export interface ConfirmRequest {
  title: string;
  message: string;
  confirmText: string;
  danger: boolean;
  resolve: (ok: boolean) => void;
}

export const confirmState = ref<ConfirmRequest | null>(null);

export interface ConfirmOptions {
  confirmText?: string;
  danger?: boolean;
}

/** 弹出确认框；返回 Promise<boolean>（确定 = true，取消 / 关闭 = false） */
export function askConfirm(
  title: string,
  message: string,
  options: ConfirmOptions = {},
): Promise<boolean> {
  // 已有未决的确认框：先按取消处理，避免 Promise 永久挂起
  if (confirmState.value) {
    confirmState.value.resolve(false);
  }
  return new Promise<boolean>((resolve) => {
    confirmState.value = {
      title,
      message,
      confirmText: options.confirmText ?? "确定",
      danger: options.danger ?? false,
      resolve,
    };
  });
}

export function resolveConfirm(ok: boolean) {
  const pending = confirmState.value;
  confirmState.value = null;
  pending?.resolve(ok);
}
