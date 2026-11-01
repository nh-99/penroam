<script setup lang="ts">
// Dropdown menu (role=menu). Opening it counts as a canvas overlay.
import { onBeforeUnmount, ref, watch } from "vue";
import { useUi } from "../../stores/ui";

const props = defineProps<{ label: string; align?: "left" | "right"; up?: boolean }>();
const open = ref(false);
const root = ref<HTMLElement | null>(null);
const ui = useUi();

watch(open, (v, old) => {
  if (v && !old) ui.overlays++;
  if (!v && old) ui.overlays--;
});
function onDoc(e: PointerEvent) {
  if (open.value && root.value && !root.value.contains(e.target as Node)) open.value = false;
}
document.addEventListener("pointerdown", onDoc, true);
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onDoc, true);
  if (open.value) ui.overlays--;
});
function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") open.value = false;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    const items = Array.from(root.value?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? []);
    const i = items.indexOf(document.activeElement as HTMLElement);
    items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
    e.preventDefault();
  }
}
const close = () => (open.value = false);
void props;
</script>

<template>
  <div ref="root" class="relative" @keydown="onKey">
    <slot name="trigger" :open="open" :toggle="() => (open = !open)" />
    <div
      v-if="open"
      role="menu"
      :aria-label="label"
      class="absolute z-40 min-w-56 rounded-xl border border-line bg-surface p-1.5 shadow-lg epaper:border-2"
      :class="[align === 'left' ? 'left-0' : 'right-0', up ? 'bottom-full mb-2' : 'top-full mt-2']"
      @click="close"
    >
      <slot :close="close" />
    </div>
  </div>
</template>
