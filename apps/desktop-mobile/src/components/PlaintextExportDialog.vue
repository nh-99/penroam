<script setup lang="ts">
// Sync keeps notes end-to-end encrypted; an exported file isn't. Exporting is the user's call,
// so this only makes sure they know before the file exists.
import { ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { useSettings } from "../stores/settings";

const emit = defineEmits<{ answer: [ok: boolean] }>();
const settings = useSettings();
const dontAsk = ref(false);

function go() {
  if (dontAsk.value) settings.s.plaintextExportOk = true;
  emit("answer", true);
}
</script>

<template>
  <Modal title="Export without encryption?" @close="emit('answer', false)">
    <p class="text-sm leading-relaxed text-muted">
      Your synced notes are end-to-end encrypted. This file won't be, so anyone who gets it can read what's in it. Save it somewhere you trust.
    </p>
    <label class="mt-3 flex items-center gap-2 text-sm"><input v-model="dontAsk" type="checkbox" />Don't ask again</label>
    <template #actions>
      <Btn autofocus @click="emit('answer', false)">Cancel</Btn>
      <Btn variant="primary" @click="go">Export</Btn>
    </template>
  </Modal>
</template>
