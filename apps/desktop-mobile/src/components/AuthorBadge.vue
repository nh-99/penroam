<script setup lang="ts">
// Who made a change: initials, a shape and the name in words, so it is never colour alone.
// Unknown, former and "before sharing" authors say so rather than showing a made-up name.
import { computed } from "vue";
import { authorLabel, type AuthorView } from "../platform/history";
import { peerInitials, peerShape } from "../platform/live";

const props = defineProps<{ author: AuthorView }>();
const label = computed(() => authorLabel(props.author));
const named = computed(() => props.author.kind === "member" && !!props.author.display.trim());
const initials = computed(() => (named.value ? peerInitials({ memberDisplay: props.author.display }) : "?"));
// a stable shape per person, so two authors can be told apart without colour
const shape = computed(() => {
  const key = props.author.accountId ?? props.author.kind;
  let h = 0;
  for (const c of key) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return peerShape(h);
});
</script>

<template>
  <span class="inline-flex items-center gap-1 text-xs" data-testid="author">
    <span class="inline-flex h-5 min-w-5 items-center justify-center rounded-full border border-line bg-surface-2 px-1 text-[10px] font-semibold epaper:border-black" aria-hidden="true">{{ initials }}</span>
    <span aria-hidden="true">{{ shape }}</span>
    <bdi>{{ label }}</bdi>
  </span>
</template>
