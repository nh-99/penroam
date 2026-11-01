<script setup lang="ts">
defineProps<{ variant?: "primary" | "ghost" | "danger" | "outline"; disabled?: boolean; type?: "button" | "submit" }>();
</script>

<template>
  <button
    :type="type ?? 'button'"
    :disabled="disabled"
    class="touch-target inline-flex items-center justify-center gap-2 rounded-xl px-4 text-sm font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40 epaper:border-2 epaper:border-black"
    :class="{
      'bg-accent text-on-accent hover:opacity-90': variant === 'primary',
      'text-ink hover:bg-surface-2': variant === 'ghost' || !variant,
      'border border-line text-ink hover:bg-surface-2': variant === 'outline',
      'bg-danger text-white hover:opacity-90 epaper:bg-black': variant === 'danger',
    }"
  >
    <slot />
  </button>
</template>
