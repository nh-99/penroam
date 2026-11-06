<script setup lang="ts">
// Pen toolbar: tools, colour, thickness, undo/redo, zoom. Shown as a strip
// under the top bar on wide screens and as a bottom tray on narrow ones (it
// never floats over the canvas, so native ink overlays can't cover it).
import { computed } from "vue";
import { Pen, Highlighter, Eraser, MousePointer2, LassoSelect, Type, Hand, Undo2, Redo2, ZoomIn, ZoomOut, Scan, Trash, SquarePen, Rows3 } from "@lucide/vue";
import IconBtn from "./ui/IconBtn.vue";
import type { EditorState, Tool } from "../editor/engine";
import { HIGHLIGHTER_COLORS, HIGHLIGHTER_WIDTHS, PEN_COLORS, PEN_WIDTHS, TEXT_SIZES, useSettings } from "../stores/settings";
import { modKey } from "../platform/device";

const props = defineProps<{ state: EditorState; compact?: boolean }>();
const emit = defineEmits<{ tool: [t: Tool]; undo: []; redo: []; zoomIn: []; zoomOut: []; fit: []; deleteSelection: []; editText: []; restyle: [s: { color?: string; fontSize?: number }]; freePlacement: [free: boolean] }>();
const settings = useSettings();
const mod = modKey();

const tools: { t: Tool; label: string; icon: any; key: string }[] = [
  { t: "pen", label: "Pen", icon: Pen, key: "P" },
  { t: "highlighter", label: "Highlighter", icon: Highlighter, key: "M" },
  { t: "eraser", label: "Stroke eraser", icon: Eraser, key: "E" },
  { t: "select", label: "Select and move", icon: MousePointer2, key: "V" },
  { t: "lasso", label: "Lasso select", icon: LassoSelect, key: "L" },
  { t: "text", label: "Text", icon: Type, key: "T" },
  { t: "hand", label: "Pan", icon: Hand, key: "H" },
];

const selecting = computed(() => props.state.tool === "select" || props.state.tool === "lasso");
const colors = computed(() => (props.state.tool === "highlighter" ? HIGHLIGHTER_COLORS : PEN_COLORS));
const currentColor = computed(() =>
  props.state.tool === "highlighter" ? settings.s.highlighterColor : props.state.tool === "text" ? settings.s.textColor : settings.s.penColor,
);
const showStyle = computed(() => ["pen", "highlighter", "text"].includes(props.state.tool) || (selecting.value && props.state.selectionCount > 0));

function setColor(c: string) {
  const t = props.state.tool;
  if (t === "highlighter") settings.s.highlighterColor = c;
  else if (t === "text") settings.s.textColor = c;
  else if (selecting.value) emit("restyle", { color: c });
  else settings.s.penColor = c;
}
function isActiveColor(c: string) {
  return !selecting.value && currentColor.value === c;
}
const sizes = computed(() => {
  const t = props.state.tool;
  if (t === "highlighter") return HIGHLIGHTER_WIDTHS.map((w) => ({ v: w, active: settings.s.highlighterWidth === w, dot: Math.min(18, w * 0.7) }));
  if (t === "text") return TEXT_SIZES.map((v) => ({ v, active: settings.s.textSize === v, dot: 0 }));
  if (t === "pen") return PEN_WIDTHS.map((w) => ({ v: w, active: settings.s.penWidth === w, dot: Math.max(3, w * 2) }));
  return [];
});
// Text locks to ruled and dotted paper's rows. With the Text tool this button sets how new text
// is placed; with text selected it changes the selection.
const showRowLock = computed(() => props.state.pageHasRows && (props.state.tool === "text" || (selecting.value && props.state.selectionHasText)));
const rowLocked = computed(() => (selecting.value ? !props.state.selectionTextFree : !settings.s.textFreePlacement));
function toggleRowLock() {
  const free = rowLocked.value;
  if (selecting.value) emit("freePlacement", free);
  else settings.s.textFreePlacement = free;
}
function setSize(v: number) {
  const t = props.state.tool;
  if (t === "highlighter") settings.s.highlighterWidth = v;
  else if (t === "text") settings.s.textSize = v;
  else settings.s.penWidth = v;
}
</script>

<template>
  <div class="flex items-center gap-1 overflow-x-auto px-2 py-1.5 [scrollbar-width:none]" role="toolbar" aria-label="Pen tools">
    <div class="flex items-center gap-0.5" role="radiogroup" aria-label="Tool">
      <IconBtn v-for="x in tools" :key="x.t" :label="x.label" :shortcut="x.key" :active="state.tool === x.t" role="radio" :aria-checked="state.tool === x.t" @click="emit('tool', x.t)">
        <component :is="x.icon" :size="20" />
      </IconBtn>
    </div>

    <template v-if="showStyle">
      <div class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
      <div class="flex items-center gap-1" role="group" aria-label="Colour">
        <button v-for="c in colors" :key="c" type="button" class="touch-target inline-flex shrink-0 items-center justify-center rounded-full"
          :aria-label="`Colour ${c}`" :aria-pressed="isActiveColor(c)" :title="c" @click="setColor(c)">
          <span class="h-6 w-6 rounded-full border border-black/15" :class="isActiveColor(c) ? 'ring-2 ring-accent ring-offset-2 ring-offset-surface epaper:ring-black' : ''" :style="{ background: c }" />
        </button>
      </div>
      <template v-if="sizes.length">
        <div class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
        <div class="flex items-center gap-0.5" role="group" :aria-label="state.tool === 'text' ? 'Text size' : 'Thickness'">
          <button v-for="s in sizes" :key="s.v" type="button" class="touch-target inline-flex shrink-0 items-center justify-center rounded-xl hover:bg-surface-2"
            :class="s.active ? 'bg-brand-soft ring-1 ring-brand/40 epaper:bg-black epaper:text-white' : ''"
            :aria-pressed="s.active" :aria-label="state.tool === 'text' ? `Text size ${s.v} pt` : `Thickness ${s.v} pt`" @click="setSize(s.v)">
            <span v-if="s.dot" class="rounded-full bg-current" :style="{ width: `${s.dot}px`, height: `${s.dot}px` }" />
            <span v-else class="text-xs font-semibold">{{ s.v }}</span>
          </button>
        </div>
      </template>
    </template>

    <template v-if="showRowLock">
      <div class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
      <IconBtn :label="rowLocked ? 'Text locked to rows (click for free placement)' : 'Free text placement (click to lock to rows)'" :active="rowLocked" role="switch" :aria-checked="rowLocked" @click="toggleRowLock"><Rows3 :size="20" /></IconBtn>
    </template>

    <template v-if="selecting && state.selectionCount > 0">
      <div class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
      <IconBtn v-if="state.selectionCount === 1 && state.selectionHasText" label="Edit text" shortcut="Enter" @click="emit('editText')"><SquarePen :size="20" /></IconBtn>
      <IconBtn label="Delete selection" shortcut="Del" @click="emit('deleteSelection')"><Trash :size="20" /></IconBtn>
    </template>

    <div class="ml-auto flex shrink-0 items-center gap-0.5 pl-2">
      <IconBtn label="Undo" :shortcut="`${mod}+Z`" :disabled="!state.canUndo" @click="emit('undo')"><Undo2 :size="20" /></IconBtn>
      <IconBtn label="Redo" :shortcut="`${mod}+Shift+Z`" :disabled="!state.canRedo" @click="emit('redo')"><Redo2 :size="20" /></IconBtn>
      <template v-if="!compact">
        <div class="mx-1.5 h-7 w-px bg-line" aria-hidden="true" />
        <IconBtn label="Zoom out" :shortcut="`${mod}+−`" @click="emit('zoomOut')"><ZoomOut :size="20" /></IconBtn>
        <button class="touch-target min-w-14 rounded-xl px-1 text-xs tabular-nums text-muted hover:bg-surface-2" :aria-label="`Zoom ${Math.round(state.scale * 100)}%, fit width`" title="Fit width" @click="emit('fit')">
          {{ Math.round(state.scale * 100) }}%
        </button>
        <IconBtn label="Zoom in" :shortcut="`${mod}+=`" @click="emit('zoomIn')"><ZoomIn :size="20" /></IconBtn>
        <IconBtn label="Fit page width" :shortcut="`${mod}+0`" @click="emit('fit')"><Scan :size="20" /></IconBtn>
      </template>
    </div>
  </div>
</template>
