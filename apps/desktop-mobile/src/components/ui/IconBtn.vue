<script setup lang="ts">
// Icon-only control: always has an accessible name and a tooltip.
defineProps<{ label: string; active?: boolean; disabled?: boolean; shortcut?: string }>();
</script>

<template>
  <button
    type="button"
    :aria-label="label"
    :title="shortcut ? `${label} (${shortcut})` : label"
    :aria-pressed="active === undefined ? undefined : active"
    :disabled="disabled"
    class="touch-target inline-flex shrink-0 items-center justify-center rounded-xl text-ink transition-colors hover:bg-surface-2 disabled:cursor-not-allowed disabled:opacity-35"
    :class="active ? 'bg-brand-soft text-brand ring-1 ring-brand/40 epaper:bg-black epaper:text-white epaper:ring-0' : ''"
  >
    <slot />
  </button>
</template>
