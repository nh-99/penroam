<script setup lang="ts">
// Draws one guided tour (stores/tour.ts): the window dims except for the control a step is about,
// and a card beside it says what the control does. The tour blocks the rest of the app while it's
// open, counts as an overlay (so native ink layers hide), and closes with Escape. Steps whose
// target isn't on screen are left out when the tour opens (see tourSteps.ts).
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import Btn from "./ui/Btn.vue";
import { TOURS, type TourStep } from "./tourSteps";
import { useTour, type TourName } from "../stores/tour";
import { useUi } from "../stores/ui";
import { useSettings } from "../stores/settings";

const props = defineProps<{ name: TourName }>();
const tour = useTour();
const ui = useUi();
const settings = useSettings();

/** Space between the highlighted control and the ring around it, in CSS pixels. */
const PAD = 6;
/** Card width; the card never gets closer than MARGIN to the window's edge. */
const CARD_W = 320;
const MARGIN = 16;

interface Shown {
  step: TourStep;
  target: string | null;
}

const steps = ref<Shown[]>([]);
const index = ref(0);
const rect = ref<DOMRect | null>(null);
const view = ref({ w: window.innerWidth, h: window.innerHeight });
const nextBtn = ref<InstanceType<typeof Btn> | null>(null);
const titleId = `tour-${Math.random().toString(36).slice(2)}`;

/** The first selector of `step` that matches an element with a size on screen. */
function findTarget(step: TourStep): string | null {
  for (const sel of step.target) {
    const el = document.querySelector<HTMLElement>(sel);
    if (el && el.getClientRects().length && el.getBoundingClientRect().width > 0) return sel;
  }
  return null;
}

const current = computed(() => steps.value[index.value] ?? null);
const last = computed(() => index.value === steps.value.length - 1);

function measure() {
  view.value = { w: window.innerWidth, h: window.innerHeight };
  const sel = current.value?.target;
  rect.value = sel ? (document.querySelector(sel)?.getBoundingClientRect() ?? null) : null;
}

async function show(i: number) {
  index.value = i;
  measure();
  await nextTick();
  (nextBtn.value?.$el as HTMLElement | undefined)?.focus();
}

function next() {
  if (last.value) tour.stop(true);
  else void show(index.value + 1);
}

function back() {
  if (index.value > 0) void show(index.value - 1);
}

/** The ring around the target, padded. */
const ring = computed(() => {
  const r = rect.value;
  if (!r) return null;
  return { left: r.left - PAD, top: r.top - PAD, width: r.width + 2 * PAD, height: r.height + 2 * PAD };
});

/**
 * Where the card goes: below the target if there's room, else above it; beside a tall target such
 * as the page sidebar; in the middle of the window when there's no target.
 */
const cardStyle = computed(() => {
  const { w, h } = view.value;
  const width = Math.min(CARD_W, w - 2 * MARGIN);
  const g = ring.value;
  if (!g) return { width: `${width}px`, left: `${(w - width) / 2}px`, top: "50%", transform: "translateY(-50%)" };
  const clampX = (x: number) => Math.max(MARGIN, Math.min(x, w - MARGIN - width));
  if (g.height > h / 2 && g.left + g.width + width + 2 * MARGIN <= w) {
    return { width: `${width}px`, left: `${g.left + g.width + MARGIN}px`, top: `${Math.max(MARGIN, g.top + 40)}px` };
  }
  const left = `${clampX(g.left + g.width / 2 - width / 2)}px`;
  if (g.top + g.height + 240 < h) return { width: `${width}px`, left, top: `${g.top + g.height + 12}px` };
  return { width: `${width}px`, left, bottom: `${h - g.top + 12}px` };
});

/** Dims everything outside the ring. ePaper has no grey, so it pales the rest instead. */
const shade = computed(() => (settings.effectiveTheme() === "epaper" ? "rgba(255,255,255,0.75)" : "rgba(0,0,0,0.5)"));

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    tour.stop(false);
  } else if (e.key === "ArrowRight") {
    next();
  } else if (e.key === "ArrowLeft") {
    back();
  }
}

onMounted(async () => {
  ui.overlays++;
  window.addEventListener("resize", measure);
  // capture, so the tour's keys never reach the editor or the library underneath
  window.addEventListener("keydown", onKey, true);
  // a view that just opened (the editor, after a notebook loads) may not have laid out its controls yet
  await nextTick();
  await new Promise((r) => setTimeout(r, 250));
  steps.value = TOURS[props.name].flatMap((step): Shown[] => {
    if (!step.target.length) return [{ step, target: null }];
    const target = findTarget(step);
    return target ? [{ step, target }] : [];
  });
  if (!steps.value.length) tour.stop(true);
  else void show(0);
});

onBeforeUnmount(() => {
  ui.overlays--;
  window.removeEventListener("resize", measure);
  window.removeEventListener("keydown", onKey, true);
});
</script>

<template>
  <div v-if="current" class="fixed inset-0 z-[60]" @pointerdown.prevent>
    <div v-if="ring" class="pointer-events-none absolute rounded-xl ring-2 ring-brand transition-all duration-200 epaper:ring-black"
      :style="{ left: `${ring.left}px`, top: `${ring.top}px`, width: `${ring.width}px`, height: `${ring.height}px`, boxShadow: `0 0 0 9999px ${shade}` }" />
    <div v-else class="absolute inset-0" :style="{ background: shade }" />
    <div role="dialog" aria-modal="true" :aria-labelledby="titleId" aria-live="polite"
      class="absolute rounded-2xl border border-line bg-surface p-4 shadow-xl epaper:border-2 epaper:border-black epaper:shadow-none"
      :style="cardStyle" @pointerdown.stop>
      <p class="text-xs text-muted">{{ index + 1 }} of {{ steps.length }}</p>
      <h2 :id="titleId" class="mt-1 text-base font-semibold">{{ current.step.title }}</h2>
      <p class="mt-1.5 text-sm leading-relaxed text-muted epaper:text-ink">{{ current.step.body }}</p>
      <div class="mt-4 flex items-center gap-2">
        <button v-if="!last" type="button" class="mr-auto rounded-lg px-2 py-1 text-sm text-muted hover:bg-surface-2" @click="tour.stop(false)">Skip tour</button>
        <span v-else class="mr-auto" />
        <Btn v-if="index > 0" @click="back">Back</Btn>
        <Btn ref="nextBtn" variant="primary" @click="next">{{ last ? "Done" : "Next" }}</Btn>
      </div>
    </div>
  </div>
</template>
