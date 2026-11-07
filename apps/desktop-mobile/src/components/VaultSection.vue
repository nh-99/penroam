<script setup lang="ts">
// Vaults: notebooks whose keys Penroam never holds. A vault's passphrase is needed once per
// device; after that the device keeps its own keys in the system keychain.
import { computed, onMounted, ref } from "vue";
import { Copy, KeyRound, Lock, LockOpen, Plus, Trash2 } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import Modal from "./ui/Modal.vue";
import { useSync } from "../stores/sync";
import { useUi } from "../stores/ui";
import { codeGroups, MIN_PASSPHRASE_CHARS, type SyncVault, type VaultSecret } from "../platform/sync";

const sync = useSync();
const ui = useUi();

/** The open form: a new vault, unlocking, or changing a passphrase. */
const form = ref<{ kind: "create" } | { kind: "unlock" | "change"; vault: SyncVault } | null>(null);
const deleting = ref<SyncVault | null>(null);
const name = ref("");
const secret = ref("");
const useRecovery = ref(false);
const next = ref("");
const again = ref("");
const confirmGroup = ref("");

const groups = computed(() => codeGroups(sync.vaultRecoveryCode ?? ""));
const lastGroup = computed(() => groups.value[groups.value.length - 1] ?? "");
const confirmed = computed(() => lastGroup.value !== "" && confirmGroup.value.trim().toUpperCase() === lastGroup.value.toUpperCase());
const needsNew = computed(() => form.value?.kind !== "unlock");
const nextOk = computed(() => !needsNew.value || (next.value.length >= MIN_PASSPHRASE_CHARS && next.value === again.value));
const canSubmit = computed(() => {
  if (sync.busy || !nextOk.value) return false;
  return form.value?.kind === "create" ? name.value.trim() !== "" : secret.value.trim() !== "";
});

onMounted(() => void sync.refreshVaults());

function openForm(f: NonNullable<typeof form.value>) {
  form.value = f;
  name.value = secret.value = next.value = again.value = "";
  useRecovery.value = false;
}

function typedSecret(): VaultSecret {
  return useRecovery.value ? { recoveryCode: secret.value.trim() } : { passphrase: secret.value };
}

async function submit() {
  const f = form.value;
  if (!f) return;
  let ok = false;
  if (f.kind === "create") ok = await sync.createVault(name.value.trim(), next.value);
  if (f.kind === "unlock") ok = await sync.unlockVault(f.vault.vaultId, typedSecret());
  if (f.kind === "change") ok = await sync.setVaultPassphrase(f.vault.vaultId, typedSecret(), next.value);
  if (!ok) return;
  if (f.kind === "change") ui.success("Passphrase changed.");
  form.value = null;
  secret.value = next.value = again.value = "";
}

async function copyCode() {
  try {
    await navigator.clipboard.writeText(sync.vaultRecoveryCode ?? "");
    ui.success("Recovery key copied.");
  } catch {
    ui.error("Copying is not available here; select the text instead.");
  }
}

function saved() {
  confirmGroup.value = "";
  sync.finishVaultRecovery();
}

async function confirmDelete() {
  const v = deleting.value;
  deleting.value = null;
  if (v) await sync.deleteVault(v.vaultId);
}
</script>

<template>
  <div v-if="sync.vaults.supported" class="space-y-2">
    <h4 class="text-xs font-semibold uppercase tracking-wide text-muted">Vaults</h4>
    <p class="text-xs leading-relaxed text-muted">Notebooks in a vault are readable only with your passphrase. Penroam can't read them or reset the passphrase. You enter it once on each device.</p>

    <!-- one-time recovery key -->
    <div v-if="sync.vaultRecoveryCode" class="space-y-3 rounded-xl border-2 border-warn p-3 epaper:border-black">
      <p class="flex items-center gap-2 text-sm font-semibold"><KeyRound :size="16" />Save your recovery key</p>
      <div class="grid grid-cols-3 gap-1.5 font-mono text-base font-semibold tracking-wider sm:grid-cols-4" aria-label="Recovery key">
        <span v-for="(g, i) in groups" :key="i" class="rounded-md bg-surface-2 px-2 py-1 text-center epaper:border">{{ g }}</span>
      </div>
      <p class="text-xs leading-relaxed">If you forget the passphrase, this key is the only way back in. Penroam can't reset either one. Store it in a password manager or write it down; it's shown only once.</p>
      <Btn variant="outline" @click="copyCode"><Copy :size="16" />Copy</Btn>
      <label class="block text-xs">
        To confirm you saved it, type the last group ({{ groups.length }} of {{ groups.length }}):
        <input v-model="confirmGroup" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 font-mono uppercase" autocomplete="off" spellcheck="false" />
      </label>
      <Btn variant="primary" :disabled="!confirmed" @click="saved">I saved the key</Btn>
    </div>

    <ul v-if="sync.vaults.vaults.length" class="space-y-1.5">
      <li v-for="v in sync.vaults.vaults" :key="v.vaultId" class="flex items-center gap-2 rounded-xl border border-line p-2.5">
        <LockOpen v-if="v.unlocked" :size="16" class="shrink-0 text-muted" aria-hidden="true" />
        <Lock v-else :size="16" class="shrink-0 text-muted" aria-hidden="true" />
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm">{{ v.name ?? "Locked vault" }}</p>
          <p class="text-xs text-muted">{{ v.problem ?? (v.unlocked ? "Unlocked on this device" : "Not unlocked on this device") }}</p>
        </div>
        <Btn v-if="!v.unlocked" variant="outline" @click="openForm({ kind: 'unlock', vault: v })">Unlock</Btn>
        <template v-else>
          <button class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-label="`Change the passphrase of ${v.name ?? 'this vault'}`" @click="openForm({ kind: 'change', vault: v })"><KeyRound :size="16" /></button>
          <button class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-label="`Delete ${v.name ?? 'this vault'}`" @click="deleting = v"><Trash2 :size="16" /></button>
        </template>
      </li>
    </ul>
    <Btn variant="outline" :disabled="sync.busy" @click="openForm({ kind: 'create' })"><Plus :size="16" />New vault</Btn>

    <Modal v-if="form" :title="form.kind === 'create' ? 'New vault' : form.kind === 'unlock' ? 'Unlock vault' : 'Change passphrase'" @close="form = null">
      <form class="space-y-3" @submit.prevent="canSubmit && submit()">
        <label v-if="form.kind === 'create'" class="block text-sm">Name
          <input v-model="name" autofocus maxlength="100" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" autocomplete="off" />
        </label>
        <template v-else>
          <label class="block text-sm">{{ useRecovery ? "Recovery key" : form.kind === "change" ? "Current passphrase" : "Passphrase" }}
            <input v-if="useRecovery" v-model="secret" autofocus class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 font-mono text-base uppercase outline-none focus:border-brand epaper:border-2" autocomplete="off" spellcheck="false" placeholder="PRK1-…" />
            <input v-else v-model="secret" autofocus type="password" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" autocomplete="off" />
          </label>
          <button type="button" class="touch-target text-xs text-brand underline-offset-2 hover:underline" @click="useRecovery = !useRecovery; secret = ''">
            {{ useRecovery ? "Use the passphrase" : "Use the recovery key" }}
          </button>
        </template>
        <template v-if="needsNew">
          <label class="block text-sm">{{ form.kind === "create" ? "Passphrase" : "New passphrase" }}
            <input v-model="next" type="password" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" autocomplete="new-password" />
          </label>
          <label class="block text-sm">Type it again
            <input v-model="again" type="password" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" autocomplete="new-password" />
          </label>
          <p class="text-xs leading-relaxed text-muted">Use at least {{ MIN_PASSPHRASE_CHARS }} characters, and something you don't use anywhere else: several unrelated words work well. A weak passphrase can be guessed by anyone who gets hold of the vault's stored copy.</p>
        </template>
        <p v-if="sync.error" class="text-xs text-danger first-letter:uppercase" role="alert">{{ sync.error }}</p>
        <div class="flex justify-end gap-2 pt-1">
          <Btn @click="form = null">Cancel</Btn>
          <Btn variant="primary" type="submit" :disabled="!canSubmit">{{ form.kind === "create" ? "Create vault" : form.kind === "unlock" ? "Unlock" : "Change passphrase" }}</Btn>
        </div>
      </form>
    </Modal>

    <ConfirmDialog v-if="deleting" title="Delete this vault?" danger confirm="Delete vault"
      :message="`The cloud copy of “${deleting.name ?? 'this vault'}” is erased for every device. Its notebooks stay on the devices that have them, and stop syncing.`"
      @close="deleting = null" @confirm="confirmDelete" />
  </div>
</template>
