<script setup lang="ts">
// Moves a notebook into a vault. The dialog is plain about what can't be undone: what was already
// uploaded stays in the account's history, where it isn't private.
import { ref } from "vue";
import Btn from "./ui/Btn.vue";
import Modal from "./ui/Modal.vue";
import { useLibrary } from "../stores/library";
import { useSync } from "../stores/sync";
import type { NotebookSummary } from "../generated/NotebookSummary";

const props = defineProps<{ notebook: NotebookSummary }>();
const emit = defineEmits<{ close: [] }>();
const lib = useLibrary();
const sync = useSync();
const vault = ref(sync.openVaults[0]?.vaultId ?? "");

async function move() {
  emit("close");
  await lib.moveToVault(props.notebook.id, vault.value);
}
</script>

<template>
  <Modal :title="`Move “${notebook.title}” to a vault`" @close="emit('close')">
    <form class="space-y-4" @submit.prevent="move">
      <label class="block text-sm">Vault
        <select v-model="vault" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2">
          <option v-for="v in sync.openVaults" :key="v.vaultId" :value="v.vaultId">{{ v.name ?? "Vault" }}</option>
        </select>
      </label>
      <p class="text-xs leading-relaxed text-muted">
        A copy goes into the vault and the original leaves your other devices. The copy starts without the notebook's version history.
        What already synced stays in your account's history until you delete the account, and that history isn't private.
      </p>
      <div class="flex justify-end gap-2">
        <Btn @click="emit('close')">Cancel</Btn>
        <Btn variant="primary" type="submit" :disabled="!vault">Move to vault</Btn>
      </div>
    </form>
  </Modal>
</template>
