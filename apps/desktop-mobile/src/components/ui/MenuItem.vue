<script setup lang="ts">
defineProps<{ danger?: boolean; disabled?: boolean }>();
</script>

<template>
  <button
    type="button"
    role="menuitem"
    :disabled="disabled"
    class="flex min-h-10 w-full items-center gap-3 rounded-lg px-3 text-left text-sm hover:bg-surface-2 disabled:opacity-40 epaper:min-h-12"
    :class="danger ? 'text-danger' : 'text-ink'"
  >
    <slot />
  </button>
</template>
