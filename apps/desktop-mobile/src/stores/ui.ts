import { defineStore } from "pinia";
import { ref } from "vue";

export interface Toast {
  id: number;
  kind: "info" | "success" | "error";
  text: string;
  action?: { label: string; run: () => void };
  sticky?: boolean;
}

export const useUi = defineStore("ui", () => {
  const toasts = ref<Toast[]>([]);
  /** Count of open modal/menu layers over the canvas (hides native ink overlays). */
  const overlays = ref(0);
  let seq = 0;

  function toast(kind: Toast["kind"], text: string, opts: { action?: Toast["action"]; sticky?: boolean; ms?: number } = {}) {
    const id = ++seq;
    toasts.value.push({ id, kind, text, action: opts.action, sticky: opts.sticky });
    if (!opts.sticky) setTimeout(() => dismiss(id), opts.ms ?? (kind === "error" ? 8000 : 3500));
    return id;
  }

  function dismiss(id: number) {
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }

  /** Replaces a toast's text in place, e.g. to report progress; no-op once it's dismissed. */
  function retext(id: number, text: string) {
    const t = toasts.value.find((t) => t.id === id);
    if (t) t.text = text;
  }

  return {
    toasts,
    overlays,
    toast,
    dismiss,
    retext,
    info: (t: string) => toast("info", t),
    success: (t: string) => toast("success", t),
    error: (t: string, action?: Toast["action"]) => toast("error", t, { action }),
  };
});
