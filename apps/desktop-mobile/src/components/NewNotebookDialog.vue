<script setup lang="ts">
import { ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import TemplatePicker from "./TemplatePicker.vue";
import type { Paper } from "../generated/Paper";
import type { TemplateKind } from "../generated/TemplateKind";
import { useSettings } from "../stores/settings";
import { useSync } from "../stores/sync";

const emit = defineEmits<{ close: []; create: [title: string, paper: Paper, template: TemplateKind, vault: string | undefined, infiniteCanvas: boolean] }>();
const settings = useSettings();
const sync = useSync();
const vault = ref("");
const title = ref("");
const paper = ref<Paper>(settings.s.paper === "a4" ? "a4" : "letter");
const template = ref<TemplateKind>(settings.s.template);
const infinite = ref(settings.s.infiniteCanvas);

function submit() {
  const t = title.value.trim() || "Untitled notebook";
  settings.s.paper = paper.value;
  settings.s.template = template.value;
  settings.s.infiniteCanvas = infinite.value;
  emit("create", t, paper.value, template.value, vault.value || undefined, infinite.value);
}
</script>

<template>
  <Modal title="New notebook" @close="emit('close')">
    <form class="space-y-5" @submit.prevent="submit">
      <label class="block">
        <span class="mb-1.5 block text-sm font-medium">Title</span>
        <input v-model="title" autofocus maxlength="300" placeholder="Untitled notebook" class="h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" />
      </label>
      <fieldset>
        <legend class="mb-1.5 text-sm font-medium">Page size</legend>
        <div class="grid grid-cols-2 gap-2" role="radiogroup" aria-label="Page size">
          <button v-for="p in (['letter', 'a4'] as Paper[])" :key="p" type="button" role="radio" :aria-checked="paper === p"
            class="h-11 rounded-xl border text-sm" :class="paper === p ? 'border-brand bg-brand-soft text-brand epaper:border-2 epaper:border-black epaper:bg-white epaper:font-bold' : 'border-line hover:bg-surface-2'"
            @click="paper = p">
            {{ p === 'letter' ? 'US Letter · 8.5 × 11 in' : 'A4 · 210 × 297 mm' }}
          </button>
        </div>
      </fieldset>
      <fieldset>
        <legend class="mb-1.5 text-sm font-medium">Paper</legend>
        <TemplatePicker v-model="template" />
      </fieldset>
      <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3 epaper:border-2">
        <span>
          <span class="block text-sm font-medium">Infinite canvas</span>
          <span class="block text-xs text-muted">Pages have no edges: write in any direction and they grow with you. You can change this later.</span>
        </span>
        <input v-model="infinite" type="checkbox" class="h-5 w-5 shrink-0" />
      </label>
      <label v-if="sync.openVaults.length" class="block">
        <span class="mb-1.5 block text-sm font-medium">Keep in</span>
        <select v-model="vault" class="h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2">
          <option value="">My notebooks</option>
          <option v-for="v in sync.openVaults" :key="v.vaultId" :value="v.vaultId">{{ v.name ?? "Vault" }}</option>
        </select>
      </label>
      <div class="flex justify-end gap-2 pt-1">
        <Btn @click="emit('close')">Cancel</Btn>
        <Btn variant="primary" type="submit">Create notebook</Btn>
      </div>
    </form>
  </Modal>
</template>
