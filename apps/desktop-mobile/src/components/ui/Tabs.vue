<script setup lang="ts">
// Accessible tab bar: role=tablist with a roving tabindex, so Tab moves past the bar and the
// arrow keys, Home and End move along it. The panel is the caller's, with id
// `settings-panel-<tab id>`. It scrolls sideways rather than wrapping on narrow screens, with no
// visible scrollbar: GTK's overlay scrollbar sits on the labels and takes their clicks. Arrow
// buttons appear when the tabs don't fit, which also shows that there's more to reach.
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ChevronLeft, ChevronRight } from "@lucide/vue";
import { tabsScroll, type TabsScroll } from "./tabsScroll";

const props = defineProps<{ tabs: { id: string; label: string }[]; modelValue: string; label: string }>();
const emit = defineEmits<{ "update:modelValue": [id: string] }>();
const bar = ref<HTMLElement | null>(null);
const scroll = ref<TabsScroll>({ overflowing: false, canLeft: false, canRight: false });
let observer: ResizeObserver | null = null;

function measure() {
  const el = bar.value;
  if (el) scroll.value = tabsScroll(el.scrollLeft, el.clientWidth, el.scrollWidth);
}

/** Moves most of a bar's width, so the tab at the edge stays in view for context. */
function nudge(direction: -1 | 1) {
  const el = bar.value;
  el?.scrollBy({ left: direction * el.clientWidth * 0.7, behavior: "smooth" });
}

function showSelected() {
  bar.value?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView?.({ inline: "nearest", block: "nearest", behavior: "smooth" });
}

onMounted(() => {
  measure();
  if (typeof ResizeObserver !== "undefined" && bar.value) {
    observer = new ResizeObserver(measure);
    observer.observe(bar.value);
  }
});
onBeforeUnmount(() => observer?.disconnect());
// the tab list changes when sync or search report in, after the bar has been measured
watch(() => props.tabs.length, () => void nextTick(measure));
watch(() => props.modelValue, () => void nextTick(showSelected));

async function onKey(e: KeyboardEvent) {
  const i = props.tabs.findIndex((t) => t.id === props.modelValue);
  const n = props.tabs.length;
  const next = { ArrowRight: (i + 1) % n, ArrowLeft: (i - 1 + n) % n, Home: 0, End: n - 1 }[e.key];
  if (next === undefined) return;
  e.preventDefault();
  emit("update:modelValue", props.tabs[next].id);
  await nextTick();
  bar.value?.querySelector<HTMLElement>('[aria-selected="true"]')?.focus();
}
</script>

<template>
  <div class="flex items-center gap-0.5 border-b border-line">
    <!-- tabindex -1: the keyboard already moves along the tabs, and the selected one scrolls into view -->
    <button v-if="scroll.overflowing" type="button" tabindex="-1" aria-label="Scroll tabs left" :disabled="!scroll.canLeft"
      class="shrink-0 rounded-md p-1 text-muted hover:text-ink disabled:opacity-30" @click="nudge(-1)">
      <ChevronLeft :size="16" />
    </button>
    <div ref="bar" role="tablist" :aria-label="label"
      class="flex min-w-0 flex-1 gap-0.5 overflow-x-auto overflow-y-hidden overscroll-x-contain [scrollbar-width:none] [touch-action:pan-x] [&::-webkit-scrollbar]:hidden"
      @keydown="onKey" @scroll.passive="measure">
      <button v-for="t in tabs" :id="`settings-tab-${t.id}`" :key="t.id" type="button" role="tab"
        :aria-selected="t.id === modelValue" :aria-controls="`settings-panel-${t.id}`" :tabindex="t.id === modelValue ? 0 : -1"
        class="shrink-0 border-b-2 px-2.5 py-2 text-sm"
        :class="t.id === modelValue ? 'border-brand font-medium text-brand epaper:border-black epaper:font-bold epaper:text-black' : 'border-transparent text-muted hover:text-ink'"
        @click="emit('update:modelValue', t.id)">
        {{ t.label }}
      </button>
    </div>
    <button v-if="scroll.overflowing" type="button" tabindex="-1" aria-label="Scroll tabs right" :disabled="!scroll.canRight"
      class="shrink-0 rounded-md p-1 text-muted hover:text-ink disabled:opacity-30" @click="nudge(1)">
      <ChevronRight :size="16" />
    </button>
  </div>
</template>
