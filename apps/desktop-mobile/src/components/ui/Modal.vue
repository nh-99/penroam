<script setup lang="ts">
// Accessible modal: role=dialog, focus moves in and is restored on close,
// Escape closes, Tab stays inside. Counts as a canvas overlay so native ink
// layers (which sit above the WebView) hide while it is open.
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { X } from "@lucide/vue";
import { useUi } from "../../stores/ui";

const props = defineProps<{ title: string; wide?: boolean }>();
const emit = defineEmits<{ close: [] }>();
const ui = useUi();
const panel = ref<HTMLElement | null>(null);
let restore: HTMLElement | null = null;
const titleId = `dlg-${Math.random().toString(36).slice(2)}`;

function focusables(): HTMLElement[] {
  return Array.from(panel.value?.querySelectorAll<HTMLElement>('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])') ?? []).filter(
    (el) => !el.hasAttribute("disabled"),
  );
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.stopPropagation();
    emit("close");
  } else if (e.key === "Tab") {
    const f = focusables();
    if (!f.length) return;
    const first = f[0], last = f[f.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }
}

onMounted(async () => {
  ui.overlays++;
  restore = document.activeElement as HTMLElement | null;
  await nextTick();
  const auto = panel.value?.querySelector<HTMLElement>("[autofocus]") ?? focusables()[1] ?? focusables()[0];
  auto?.focus();
});
onBeforeUnmount(() => {
  ui.overlays--;
  restore?.focus?.();
});
void props;
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-end justify-center bg-black/30 p-0 sm:items-center sm:p-4 epaper:bg-white/70" @pointerdown.self="emit('close')" @keydown="onKey">
    <div
      ref="panel"
      role="dialog"
      aria-modal="true"
      :aria-labelledby="titleId"
      class="max-h-[90vh] w-full overflow-y-auto rounded-t-2xl border border-line bg-surface p-5 pb-[calc(1.25rem+env(safe-area-inset-bottom))] shadow-xl sm:rounded-2xl epaper:border-2"
      :class="wide ? 'sm:max-w-2xl' : 'sm:max-w-md'"
    >
      <div class="mb-4 flex items-center justify-between gap-4">
        <h2 :id="titleId" class="text-lg font-semibold">{{ title }}</h2>
        <button class="touch-target -mr-2 inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" aria-label="Close" @click="emit('close')">
          <X :size="20" />
        </button>
      </div>
      <slot />
      <div v-if="$slots.actions" class="mt-6 flex flex-wrap justify-end gap-2">
        <slot name="actions" />
      </div>
    </div>
  </div>
</template>
