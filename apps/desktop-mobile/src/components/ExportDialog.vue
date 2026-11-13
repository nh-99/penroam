<script setup lang="ts">
// Exporting a shared notebook as a .penroam file: comments and authors stay out unless asked for,
// because the file can be passed on and would carry what collaborators wrote and their addresses.
import { ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";

defineProps<{ title: string }>();
const emit = defineEmits<{ close: []; confirm: [options: { includeComments: boolean; includeAuthors: boolean }] }>();
const include = ref(false);
const authors = ref(false);
</script>

<template>
  <Modal :title="`Export “${title}”`" @close="emit('close')">
    <label class="flex items-start gap-3 rounded-xl border border-line p-3 text-sm">
      <input v-model="include" type="checkbox" class="mt-0.5 h-5 w-5 shrink-0" />
      <span><span class="block font-medium">Include comments</span><span class="block text-xs text-muted">Off by default. The file would contain your collaborators' comment text and their account identifiers, readable by anyone you give it to, and that can't be taken back.</span></span>
    </label>
    <p v-if="include" class="mt-2 text-xs text-danger" role="note" data-testid="comments-warning">The file will hold your collaborators' comments and their account identifiers. Only give it to people who may read the discussion.</p>
    <label class="mt-3 flex items-start gap-3 rounded-xl border border-line p-3 text-sm">
      <input v-model="authors" type="checkbox" class="mt-0.5 h-5 w-5 shrink-0" />
      <span><span class="block font-medium">Include authors</span><span class="block text-xs text-muted">Off by default. Records who wrote each change, by email address. Without it the file holds the notebook only.</span></span>
    </label>
    <p v-if="authors" class="mt-2 text-xs text-danger" role="note" data-testid="authors-warning">Anyone who gets the file can see who wrote what, and the addresses can't be taken back.</p>
    <template #actions>
      <Btn @click="emit('close')">Cancel</Btn>
      <Btn variant="primary" @click="emit('confirm', { includeComments: include, includeAuthors: authors })">Export…</Btn>
    </template>
  </Modal>
</template>
