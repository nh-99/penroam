<script setup lang="ts">
// Accessible tab bar: role=tablist with a roving tabindex, so Tab moves past the bar and the
// arrow keys, Home and End move along it. The panel is the caller's, with id
// `settings-panel-<tab id>`. It scrolls sideways rather than wrapping on narrow screens.
import { nextTick, ref } from "vue";

const props = defineProps<{ tabs: { id: string; label: string }[]; modelValue: string; label: string }>();
const emit = defineEmits<{ "update:modelValue": [id: string] }>();
const bar = ref<HTMLElement | null>(null);

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
  <div ref="bar" role="tablist" :aria-label="label" class="flex gap-1 overflow-x-auto border-b border-line" @keydown="onKey">
    <button v-for="t in tabs" :id="`settings-tab-${t.id}`" :key="t.id" type="button" role="tab"
      :aria-selected="t.id === modelValue" :aria-controls="`settings-panel-${t.id}`" :tabindex="t.id === modelValue ? 0 : -1"
      class="-mb-px shrink-0 border-b-2 px-3 py-2 text-sm"
      :class="t.id === modelValue ? 'border-brand font-medium text-brand epaper:border-black epaper:font-bold epaper:text-black' : 'border-transparent text-muted hover:text-ink'"
      @click="emit('update:modelValue', t.id)">
      {{ t.label }}
    </button>
  </div>
</template>
