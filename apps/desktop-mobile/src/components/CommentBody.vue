<script setup lang="ts">
// A comment's text. Always printed as text: the body is split into plain text, mention chips and
// http(s) links, and each part goes through `{{ }}`. Markup in a comment shows as markup; nothing
// here uses v-html. Other links (javascript:, data:, …) are not links.
import { computed } from "vue";
import { bodySegments, mentionName, type Mentionable } from "../platform/comments";
import { api } from "../platform/ipc";

const props = defineProps<{ body: string; members?: Mentionable[]; authors?: Mentionable[] }>();
const parts = computed(() => bodySegments(props.body));
// the name as it is now: a listed member, or an author of this thread; otherwise a former member
const nameOf = (id: string) => mentionName(id, props.members, props.authors);
const open = (href: string) => void api.openLink(href).catch(() => {});
</script>

<template>
  <p class="whitespace-pre-wrap break-words text-sm" dir="auto" data-testid="comment-body">
    <template v-for="(p, i) in parts" :key="i">
      <span v-if="p.t === 'text'">{{ p.s }}</span>
      <span v-else-if="p.t === 'mention'" class="mx-0.5 inline-flex items-center rounded-full border border-line bg-brand-soft px-1.5 text-xs font-medium epaper:border-black" data-testid="mention-chip">@<bdi>{{ nameOf(p.id) }}</bdi></span>
      <template v-else><a :href="p.href" class="underline" rel="noopener noreferrer nofollow" target="_blank" @click.prevent="open(p.href)">{{ p.s }}</a><bdi v-if="p.host" class="ml-1 rounded bg-surface-2 px-1 text-xs font-medium epaper:border" data-testid="link-host" :title="`This link goes to ${p.host}`">({{ p.host }})</bdi></template>
    </template>
  </p>
</template>
