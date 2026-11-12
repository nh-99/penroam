<script setup lang="ts">
// Pen toolbar: tools, colour, thickness, undo/redo, zoom. Shown as a strip
// under the top bar on wide screens and as a bottom tray on narrow ones (it
// never floats over the canvas, so native ink overlays can't cover it). When it doesn't fit it
// scrolls sideways, with arrows at each end because there is no scrollbar to show that it can.
import { computed, ref } from "vue";
import { Pen, Highlighter, Eraser, MousePointer2, LassoSelect, Type, Hand, Undo2, Redo2, ZoomIn, ZoomOut, Scan, Trash, SquarePen, Rows3, Bold, Italic, Strikethrough, Code, Sigma, Heading1, Heading2, Heading3, List, ListOrdered, ListChecks, Quote, SquareCode, Link, Minus, Table, ChevronLeft, ChevronRight, Palette, Ruler } from "@lucide/vue";
import IconBtn from "./ui/IconBtn.vue";
import ColorDialog from "./ColorDialog.vue";
import type { EditorState, Tool } from "../editor/engine";
import type { FormatAction } from "../editor/markdownFormat";
import { HIGHLIGHTER_WIDTHS, PEN_WIDTHS, TEXT_SIZES, useSettings } from "../stores/settings";
import { modKey } from "../platform/device";
import type { TextFont } from "../generated/TextFont";
import { fontFamilyOf } from "../editor/textLayout";
import { useSideScroll } from "../composables/useSideScroll";

const props = defineProps<{ state: EditorState; compact?: boolean }>();
const emit = defineEmits<{ tool: [t: Tool]; format: [a: FormatAction]; undo: []; redo: []; zoomIn: []; zoomOut: []; fit: []; deleteSelection: []; editText: []; restyle: [s: { color?: string; fontSize?: number; font?: TextFont }]; freePlacement: [free: boolean]; ruler: [] }>();
const settings = useSettings();
const mod = modKey();
const strip = ref<HTMLElement | null>(null);
const { scroll, measure, nudge, handlers } = useSideScroll(strip);

const tools: { t: Tool; label: string; icon: any; key: string }[] = [
  { t: "pen", label: "Pen", icon: Pen, key: "P" },
  { t: "highlighter", label: "Highlighter", icon: Highlighter, key: "M" },
  { t: "eraser", label: "Stroke eraser", icon: Eraser, key: "E" },
  { t: "select", label: "Select and move", icon: MousePointer2, key: "V" },
  { t: "lasso", label: "Lasso select", icon: LassoSelect, key: "L" },
  { t: "text", label: "Text", icon: Type, key: "T" },
  { t: "hand", label: "Pan", icon: Hand, key: "H" },
];

// Shown while a text box is being edited. Those that insert something have nothing to light up.
const formatGroups: { a: FormatAction; label: string; icon: any; shortcut?: string; inserts?: boolean }[][] = [
  [
    { a: "bold", label: "Bold", icon: Bold, shortcut: `${mod}+B` },
    { a: "italic", label: "Italic", icon: Italic, shortcut: `${mod}+I` },
    { a: "strike", label: "Strikethrough", icon: Strikethrough },
    { a: "code", label: "Inline code", icon: Code },
    { a: "math", label: "Formula", icon: Sigma },
  ],
  [
    { a: "h1", label: "Heading 1", icon: Heading1 },
    { a: "h2", label: "Heading 2", icon: Heading2 },
    { a: "h3", label: "Heading 3", icon: Heading3 },
  ],
  [
    { a: "bullet", label: "Bulleted list", icon: List },
    { a: "ordered", label: "Numbered list", icon: ListOrdered },
    { a: "task", label: "Task list", icon: ListChecks },
    { a: "quote", label: "Quotation", icon: Quote },
    { a: "codeBlock", label: "Code block", icon: SquareCode },
  ],
  [
    { a: "link", label: "Link", icon: Link, shortcut: `${mod}+K` },
    { a: "rule", label: "Horizontal line", icon: Minus, inserts: true },
    { a: "table", label: "Table", icon: Table, inserts: true },
  ],
];

const selecting = computed(() => props.state.tool === "select" || props.state.tool === "lasso");
const colors = computed(() => settings.palette(props.state.tool === "highlighter" ? "highlighter" : "pen"));
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
const showColorDialog = ref(false);
// The tool is on a colour with no swatch of its own, so the custom button stands in for one.
const customColorActive = computed(() => !selecting.value && !colors.value.includes(currentColor.value));
function pickCustomColor(c: string) {
  setColor(c);
  showColorDialog.value = false;
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
// Two fonts: handwritten for notes that should look written, formatted for a typeset look.
// Like the row lock, this sets new text with the Text tool and changes the selection otherwise.
const fonts: { f: TextFont; label: string }[] = [
  { f: "handwritten", label: "Handwritten" },
  { f: "formatted", label: "Formatted" },
];
const showFont = computed(() => props.state.tool === "text" || (selecting.value && props.state.selectionHasText));
const activeFont = computed(() => (selecting.value ? props.state.selectionTextFont : settings.s.textFont));
function setFont(f: TextFont) {
  if (selecting.value) emit("restyle", { font: f });
  else settings.s.textFont = f;
}
function setSize(v: number) {
  const t = props.state.tool;
  if (t === "highlighter") settings.s.highlighterWidth = v;
  else if (t === "text") settings.s.textSize = v;
  else settings.s.penWidth = v;
}
</script>

<template>
  <div class="flex items-center" role="toolbar" aria-label="Pen tools" v-on="handlers">
    <!-- mousedown.prevent: taking focus would end a text edit, as with the formatting buttons -->
    <button v-if="scroll.overflowing" type="button" tabindex="-1" aria-label="Scroll toolbar left" title="Scroll left" :disabled="!scroll.canLeft" class="inline-flex w-7 shrink-0 items-center justify-center self-stretch text-muted hover:text-ink disabled:opacity-30" @mousedown.prevent @click="nudge(-1)">
      <ChevronLeft :size="20" />
    </button>
    <div ref="strip" class="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto overflow-y-hidden px-2 py-1.5 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden" @scroll.passive="measure">
      <!-- on a phone the formatting needs the room, and tapping away from the box picks tools again -->
      <div v-if="!(compact && state.editingText)" class="flex items-center gap-0.5" role="radiogroup" aria-label="Tool">
        <IconBtn v-for="x in tools" :key="x.t" :label="x.label" :shortcut="x.key" :active="state.tool === x.t" role="radio" :aria-checked="state.tool === x.t" @click="emit('tool', x.t)">
          <component :is="x.icon" :size="20" />
        </IconBtn>
      </div>
      <!-- a toggle rather than a tool: it works alongside whichever tool is active -->
      <IconBtn v-if="!(compact && state.editingText)" label="Ruler" shortcut="R" :active="state.rulerShown" @click="emit('ruler')"><Ruler :size="20" /></IconBtn>

      <!-- Pressing a button must not take focus: leaving the box ends the edit. -->
      <div v-if="state.editingText" class="flex items-center gap-0.5" role="group" aria-label="Text formatting" @mousedown.prevent>
        <template v-for="(group, i) in formatGroups" :key="i">
          <div v-if="i > 0 || !compact" class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
          <IconBtn v-for="x in group" :key="x.a" :label="x.label" :shortcut="x.shortcut" :active="x.inserts ? undefined : state.textFormats.includes(x.a)" @click="emit('format', x.a)">
            <component :is="x.icon" :size="20" />
          </IconBtn>
        </template>
      </div>

      <template v-if="showStyle && !state.editingText">
        <div class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
        <div class="flex items-center gap-1" role="group" aria-label="Colour">
          <button v-for="c in colors" :key="c" type="button" class="touch-target inline-flex shrink-0 items-center justify-center rounded-full"
            :aria-label="`Colour ${c}`" :aria-pressed="isActiveColor(c)" :title="c" @click="setColor(c)">
            <span class="h-6 w-6 rounded-full border border-black/15" :class="isActiveColor(c) ? 'ring-2 ring-accent ring-offset-2 ring-offset-surface epaper:ring-black' : ''" :style="{ background: c }" />
          </button>
          <button type="button" class="touch-target inline-flex shrink-0 items-center justify-center rounded-full text-muted hover:text-ink" aria-label="Custom colour" title="Custom colour" :aria-pressed="customColorActive" @click="showColorDialog = true">
            <span v-if="customColorActive" class="h-6 w-6 rounded-full border border-black/15 ring-2 ring-accent ring-offset-2 ring-offset-surface epaper:ring-black" :style="{ background: currentColor }" />
            <Palette v-else :size="20" />
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

      <template v-if="showFont && !state.editingText">
        <div class="mx-1.5 h-7 w-px shrink-0 bg-line" aria-hidden="true" />
        <div class="flex items-center gap-0.5" role="radiogroup" aria-label="Font">
          <button v-for="x in fonts" :key="x.f" type="button" role="radio" class="touch-target inline-flex shrink-0 items-center justify-center rounded-xl px-1 hover:bg-surface-2"
            :class="activeFont === x.f ? 'bg-brand-soft ring-1 ring-brand/40 epaper:bg-black epaper:text-white' : ''"
            :aria-checked="activeFont === x.f" :aria-label="`${x.label} font`" :title="`${x.label} font`" @click="setFont(x.f)">
            <span class="text-lg leading-none" :style="{ fontFamily: fontFamilyOf(x.f) }">Aa</span>
          </button>
        </div>
      </template>

      <template v-if="showRowLock && !state.editingText">
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
    <button v-if="scroll.overflowing" type="button" tabindex="-1" aria-label="Scroll toolbar right" title="Scroll right" :disabled="!scroll.canRight" class="inline-flex w-7 shrink-0 items-center justify-center self-stretch text-muted hover:text-ink disabled:opacity-30" @mousedown.prevent @click="nudge(1)">
      <ChevronRight :size="20" />
    </button>
    <!-- out of the toolbar's DOM, so its scrolling gestures don't act on the dialog's sliders -->
    <Teleport to="body">
      <ColorDialog v-if="showColorDialog" title="Custom colour" :color="currentColor" confirm="Use colour" @close="showColorDialog = false" @pick="pickCustomColor" />
    </Teleport>
  </div>
</template>
