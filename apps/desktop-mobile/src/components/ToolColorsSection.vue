<script setup lang="ts">
// Settings section that edits the colours the toolbar offers for each tool. The palettes are
// per-device preferences (stores/settings.ts); ink already drawn keeps the colour it has.
import { computed, ref } from "vue";
import { Plus } from "@lucide/vue";
import ColorDialog from "./ColorDialog.vue";
import Btn from "./ui/Btn.vue";
import { HIGHLIGHTER_COLORS, MAX_PALETTE_COLORS, PEN_COLORS, useSettings, type PaletteKind } from "../stores/settings";

const settings = useSettings();

const rows: { kind: PaletteKind; label: string; builtIn: string[] }[] = [
  { kind: "pen", label: "Pen and text", builtIn: PEN_COLORS },
  { kind: "highlighter", label: "Highlighter", builtIn: HIGHLIGHTER_COLORS },
];

function isBuiltIn(kind: PaletteKind, builtIn: string[]) {
  const p = settings.palette(kind);
  return p.length === builtIn.length && p.every((c, i) => c === builtIn[i]);
}

/** The swatch being edited; a null colour means a new one is being added. */
const editing = ref<{ kind: PaletteKind; label: string; color: string | null } | null>(null);
const dialogColor = computed(() => {
  if (!editing.value) return "";
  const p = settings.palette(editing.value.kind);
  return editing.value.color ?? p[p.length - 1];
});

function picked(color: string) {
  const e = editing.value;
  if (!e) return;
  if (e.color) settings.replacePaletteColor(e.kind, e.color, color);
  else settings.addPaletteColor(e.kind, color);
  editing.value = null;
}
function removed() {
  const e = editing.value;
  if (e?.color) settings.removePaletteColor(e.kind, e.color);
  editing.value = null;
}
</script>

<template>
  <section>
    <h3 class="mb-1 text-sm font-medium">Tool colours</h3>
    <p class="mb-2 text-xs text-muted">The colours shown in the toolbar. Choose one to change or remove it.</p>
    <div class="space-y-2">
      <div v-for="row in rows" :key="row.kind" class="rounded-xl border border-line p-3">
        <div class="flex min-h-8 items-center justify-between gap-2">
          <span class="text-sm">{{ row.label }}</span>
          <Btn v-if="!isBuiltIn(row.kind, row.builtIn)" variant="ghost" @click="settings.resetPalette(row.kind)">Reset</Btn>
        </div>
        <div class="flex flex-wrap items-center gap-1" role="group" :aria-label="`${row.label} colours`">
          <button v-for="c in settings.palette(row.kind)" :key="c" type="button" class="touch-target inline-flex items-center justify-center rounded-full" :aria-label="`Edit colour ${c}`" :title="c" @click="editing = { kind: row.kind, label: row.label, color: c }">
            <span class="h-6 w-6 rounded-full border border-black/15" :style="{ background: c }" />
          </button>
          <button v-if="settings.palette(row.kind).length < MAX_PALETTE_COLORS" type="button" class="touch-target inline-flex items-center justify-center rounded-full text-muted hover:text-ink" :aria-label="`Add ${row.label.toLowerCase()} colour`" title="Add colour" @click="editing = { kind: row.kind, label: row.label, color: null }">
            <Plus :size="20" />
          </button>
        </div>
      </div>
    </div>
    <Teleport to="body">
      <ColorDialog v-if="editing" :title="editing.color ? `Edit ${editing.label.toLowerCase()} colour` : `Add ${editing.label.toLowerCase()} colour`" :color="dialogColor" :confirm="editing.color ? 'Save' : 'Add'" :removable="!!editing.color && settings.palette(editing.kind).length > 1" @close="editing = null" @pick="picked" @remove="removed" />
    </Teleport>
  </section>
</template>
