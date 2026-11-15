<script setup lang="ts">
// Dropdown menu (role=menu), rendered in a fixed-position layer on <body> so
// scroll containers (page sidebar) never clip it; kept inside the viewport.
// Opening it counts as a canvas overlay (native ink layers hide meanwhile).
import { nextTick, onBeforeUnmount, ref, watch } from "vue";
import { useUi } from "../../stores/ui";

const props = defineProps<{ label: string; align?: "left" | "right"; up?: boolean }>();
const open = ref(false);
const root = ref<HTMLElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const pos = ref<{ top: number; left: number }>({ top: 0, left: 0 });
const ui = useUi();

async function place() {
  const t = root.value?.getBoundingClientRect();
  if (!t) return;
  pos.value = { top: t.bottom + 6, left: t.left };
  await nextTick();
  const p = panel.value?.getBoundingClientRect();
  if (!p) return;
  const vw = window.innerWidth, vh = window.innerHeight, m = 8;
  let left = props.align === "left" ? t.left : t.right - p.width;
  left = Math.min(Math.max(m, left), vw - p.width - m);
  let top = props.up ? t.top - p.height - 6 : t.bottom + 6;
  if (top + p.height > vh - m) top = Math.max(m, t.top - p.height - 6);
  if (top < m) top = m;
  pos.value = { top, left };
}

// Whether this menu is counted in `ui.overlays`. Tracked apart from `open` because a menu that
// closes and unmounts in the same tick (an item that hides the menu itself) never runs the
// watcher for the close, and the count would leak.
let counted = false;
watch(open, (v) => {
  if (v && !counted) {
    counted = true;
    ui.overlays++;
    void place();
  }
  if (!v && counted) {
    counted = false;
    ui.overlays--;
  }
});
function onDoc(e: PointerEvent) {
  const t = e.target as Node;
  if (open.value && !root.value?.contains(t) && !panel.value?.contains(t)) open.value = false;
}
const onWin = () => (open.value = false);
document.addEventListener("pointerdown", onDoc, true);
window.addEventListener("resize", onWin);
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDoc, true);
  window.removeEventListener("resize", onWin);
  if (counted) ui.overlays--;
});
function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    open.value = false;
    root.value?.querySelector<HTMLElement>("button")?.focus();
  }
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    const items = Array.from(panel.value?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? []);
    const i = items.indexOf(document.activeElement as HTMLElement);
    items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
    e.preventDefault();
  }
}
const close = () => (open.value = false);
</script>

<template>
  <div ref="root" class="relative" @keydown="onKey">
    <slot name="trigger" :open="open" :toggle="() => (open = !open)" />
    <Teleport to="body">
      <div
        v-if="open"
        ref="panel"
        role="menu"
        :aria-label="label"
        class="fixed z-[65] min-w-56 rounded-xl border border-line bg-surface p-1.5 shadow-lg epaper:border-2"
        :style="{ top: `${pos.top}px`, left: `${pos.left}px` }"
        @keydown="onKey"
        @click="close"
      >
        <slot :close="close" />
      </div>
    </Teleport>
  </div>
</template>
