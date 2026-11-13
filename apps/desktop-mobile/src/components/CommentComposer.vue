<script setup lang="ts">
// Writes a comment or reply. Typing `@` offers members of this notebook only (the list Rust gives
// for it), and a chosen member shows as a chip under the box. The text is sent with mention
// tokens Rust reads; nothing here renders HTML. Controls are large enough for touch and ePaper.
import { computed, nextTick, ref, watch } from "vue";
import { X } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import { commentTooLong, displayToTokens, mentionQuery, stillMentioned, suggestMentions, tokensToDisplay, type Mentionable } from "../platform/comments";

const props = defineProps<{ members: Mentionable[]; authors?: Mentionable[]; label: string; submit: string; initial?: string; busy?: boolean; autofocus?: boolean }>();
const emit = defineEmits<{ submit: [body: string]; cancel: [] }>();

const start = tokensToDisplay(props.initial ?? "", props.members, props.authors);
const text = ref(start.text);
const chosen = ref<Mentionable[]>(start.chosen);
const area = ref<HTMLTextAreaElement | null>(null);
const caret = ref(text.value.length);
const active = ref(0);
const dismissed = ref(false);
const id = `composer-${Math.random().toString(36).slice(2)}`;

const query = computed(() => (dismissed.value ? null : mentionQuery(text.value, caret.value)));
const options = computed(() => (query.value ? suggestMentions(props.members, query.value.query) : []));
const mentioned = computed(() => stillMentioned(text.value, chosen.value));
const body = computed(() => displayToTokens(text.value.trim(), mentioned.value));
const tooLong = computed(() => commentTooLong(body.value));
const canSend = computed(() => !!text.value.trim() && !tooLong.value && !props.busy);
watch([query, options], () => (active.value = 0));

function sync() {
  caret.value = area.value?.selectionStart ?? text.value.length;
  dismissed.value = false;
}
async function pick(m: Mentionable) {
  const q = query.value;
  if (!q) return;
  const before = text.value.slice(0, q.start);
  const after = text.value.slice(caret.value);
  text.value = `${before}@${m.display} ${after}`;
  if (!chosen.value.some((c) => c.accountId === m.accountId)) chosen.value = [...chosen.value, m];
  await nextTick();
  const at = before.length + m.display.length + 2;
  area.value?.setSelectionRange(at, at);
  caret.value = at;
  area.value?.focus();
}
function onKey(e: KeyboardEvent) {
  if (options.value.length) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const n = options.value.length;
      active.value = (active.value + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
      return;
    }
    if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      void pick(options.value[active.value]);
      return;
    }
    if (e.key === "Escape") {
      // closes the list only, not the composer
      e.preventDefault();
      e.stopPropagation();
      dismissed.value = true;
      return;
    }
  }
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && canSend.value) {
    e.preventDefault();
    send();
  }
}
function send() {
  if (!canSend.value) return;
  emit("submit", body.value);
  text.value = "";
  chosen.value = [];
}
function drop(m: Mentionable) {
  chosen.value = chosen.value.filter((c) => c.accountId !== m.accountId);
  text.value = text.value.replace(new RegExp(`@${m.display.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s?`, "gu"), "");
}
defineExpose({ focus: () => area.value?.focus() });
</script>

<template>
  <form class="space-y-2" @submit.prevent="send">
    <label :for="`${id}-text`" class="sr-only">{{ label }}</label>
    <div class="relative">
      <textarea
        :id="`${id}-text`"
        ref="area"
        v-model="text"
        rows="3"
        :autofocus="autofocus"
        class="w-full resize-y rounded-xl border border-line bg-surface p-3 text-base outline-none focus:border-brand epaper:border-2"
        :placeholder="`${label} — type @ to mention someone`"
        role="combobox"
        aria-autocomplete="list"
        :aria-expanded="options.length > 0"
        :aria-controls="`${id}-list`"
        :aria-activedescendant="options.length ? `${id}-opt-${active}` : undefined"
        @input="sync"
        @click="sync"
        @keyup="sync"
        @keydown="onKey"
      />
      <ul v-if="options.length" :id="`${id}-list`" role="listbox" :aria-label="`Members to mention`" class="absolute left-0 right-0 top-full z-10 mt-1 overflow-hidden rounded-xl border border-line bg-surface shadow-lg epaper:border-2" data-testid="mention-options">
        <li v-for="(m, i) in options" :id="`${id}-opt-${i}`" :key="m.accountId" role="option" :aria-selected="i === active" class="min-h-11 cursor-pointer px-3 py-2.5 text-sm" :class="i === active ? 'bg-brand-soft epaper:bg-black epaper:text-white' : ''" @pointerdown.prevent="pick(m)">
          <bdi>{{ m.display || "Member" }}</bdi>
        </li>
      </ul>
    </div>
    <ul v-if="mentioned.length" class="flex flex-wrap gap-1.5" aria-label="Mentioning">
      <li v-for="m in mentioned" :key="m.accountId" class="inline-flex items-center gap-1 rounded-full border border-line bg-brand-soft py-0.5 pl-2.5 pr-0.5 text-xs font-medium epaper:border-black" data-testid="composer-chip">
        @<bdi>{{ m.display }}</bdi>
        <button type="button" class="touch-target inline-flex items-center justify-center rounded-full" :aria-label="`Stop mentioning ${m.display}`" @click="drop(m)"><X :size="14" /></button>
      </li>
    </ul>
    <p v-if="tooLong" class="text-xs text-danger" role="alert">This comment is too long.</p>
    <div class="flex justify-end gap-2">
      <Btn type="button" @click="emit('cancel')">Cancel</Btn>
      <Btn variant="primary" type="submit" :disabled="!canSend">{{ submit }}</Btn>
    </div>
  </form>
</template>
