<script setup lang="ts">
import type { TemplateKind } from "../generated/TemplateKind";
import TemplatePreview from "./TemplatePreview.vue";

defineProps<{ modelValue: TemplateKind }>();
const emit = defineEmits<{ "update:modelValue": [v: TemplateKind] }>();
const kinds: { kind: TemplateKind; label: string }[] = [
  { kind: "blank", label: "Blank" },
  { kind: "ruled", label: "Ruled" },
  { kind: "grid", label: "Grid" },
  { kind: "dotted", label: "Dotted" },
];
</script>

<template>
  <div role="radiogroup" aria-label="Page template" class="grid grid-cols-4 gap-2">
    <button
      v-for="k in kinds"
      :key="k.kind"
      type="button"
      role="radio"
      :aria-checked="modelValue === k.kind"
      class="flex flex-col items-center gap-1.5 rounded-xl border p-2 text-xs"
      :class="modelValue === k.kind ? 'border-brand bg-brand-soft text-brand epaper:border-2 epaper:border-black epaper:bg-white epaper:font-bold' : 'border-line hover:bg-surface-2'"
      @click="emit('update:modelValue', k.kind)"
    >
      <TemplatePreview :kind="k.kind" />
      {{ k.label }}
    </button>
  </div>
</template>
