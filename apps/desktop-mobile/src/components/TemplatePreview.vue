<script setup lang="ts">
import type { TemplateKind } from "../generated/TemplateKind";
defineProps<{ kind: TemplateKind }>();
</script>

<template>
  <svg viewBox="0 0 34 44" class="h-11 w-9 rounded-sm border border-line bg-white" aria-hidden="true">
    <template v-if="kind === 'ruled'">
      <line v-for="i in 7" :key="i" x1="0" x2="34" :y1="6 + i * 5" :y2="6 + i * 5" stroke="#c7d2de" stroke-width="0.6" />
      <line x1="7" x2="7" y1="0" y2="44" stroke="#f0b4b4" stroke-width="0.6" />
    </template>
    <template v-else-if="kind === 'grid'">
      <line v-for="i in 6" :key="'v' + i" :x1="i * 5" :x2="i * 5" y1="0" y2="44" stroke="#d5dde6" stroke-width="0.5" />
      <line v-for="i in 8" :key="'h' + i" x1="0" x2="34" :y1="i * 5" :y2="i * 5" stroke="#d5dde6" stroke-width="0.5" />
    </template>
    <template v-else-if="kind === 'dotted'">
      <template v-for="i in 6" :key="'r' + i">
        <circle v-for="j in 8" :key="j" :cx="i * 5" :cy="j * 5" r="0.6" fill="#9aa7b5" />
      </template>
    </template>
  </svg>
</template>
