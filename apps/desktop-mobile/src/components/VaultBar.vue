<script setup lang="ts">
// The library's vault strip: one chip per vault, plus "New vault". Vaults hold notebooks whose
// keys Penroam never holds. A vault's passphrase is needed once per device; after that the
// device keeps its own keys in the system keychain.
import { computed, onMounted, ref } from "vue";
import { Copy, KeyRound, Lock, LockOpen, Plus, Trash2, TriangleAlert } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import Menu from "./ui/Menu.vue";
import MenuItem from "./ui/MenuItem.vue";
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
/** The recovery key dialog was put away before the key was confirmed; a chip brings it back. */
const keyHidden = ref(false);

/** How many notebooks each vault holds, as far as this device knows. */
const counts = computed(() => {
  const c: Record<string, number> = {};
  for (const v of Object.values(sync.vaults.notebooks)) c[v] = (c[v] ?? 0) + 1;
  return c;
});

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
  if (f.kind === "create") keyHidden.value = false;
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
  keyHidden.value = false;
  sync.finishVaultRecovery();
}

async function confirmDelete() {
  const v = deleting.value;
  deleting.value = null;
  if (v) await sync.deleteVault(v.vaultId);
}
</script>

<template>
  <section v-if="sync.vaults.supported" aria-labelledby="vaults-heading" class="mb-6 flex flex-wrap items-center gap-2">
    <h2 id="vaults-heading" class="mr-1 flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-muted"><Lock :size="14" aria-hidden="true" />Vaults</h2>

    <template v-for="v in sync.vaults.vaults" :key="v.vaultId">
      <button v-if="!v.unlocked" class="touch-target inline-flex items-center gap-1.5 rounded-full border border-line px-3 text-sm hover:bg-surface-2 epaper:border-2"
        :aria-label="`Unlock ${v.name ?? 'locked vault'}`" :title="v.problem ?? 'Not unlocked on this device'" @click="openForm({ kind: 'unlock', vault: v })">
        <TriangleAlert v-if="v.problem" :size="14" class="text-warn" aria-hidden="true" /><Lock v-else :size="14" class="text-muted" aria-hidden="true" />
        <span class="max-w-40 truncate">{{ v.name ?? "Locked vault" }}</span><span class="text-brand">Unlock</span>
      </button>
      <Menu v-else :label="`${v.name ?? 'Vault'} actions`" align="left">
        <template #trigger="{ toggle, open }">
          <button class="touch-target inline-flex items-center gap-1.5 rounded-full border border-line px-3 text-sm hover:bg-surface-2 epaper:border-2"
            :aria-expanded="open" aria-haspopup="menu" :title="v.problem ?? undefined" @click="toggle">
            <TriangleAlert v-if="v.problem" :size="14" class="text-warn" aria-hidden="true" /><LockOpen v-else :size="14" class="text-muted" aria-hidden="true" />
            <span class="max-w-40 truncate">{{ v.name }}</span><span class="text-muted">{{ counts[v.vaultId] ?? 0 }}</span>
          </button>
        </template>
        <MenuItem @click="openForm({ kind: 'change', vault: v })"><KeyRound :size="16" />Change passphrase…</MenuItem>
        <MenuItem danger @click="deleting = v"><Trash2 :size="16" />Delete vault…</MenuItem>
      </Menu>
    </template>

    <button v-if="sync.vaultRecoveryCode && keyHidden" class="touch-target inline-flex items-center gap-1.5 rounded-full border-2 border-warn px-3 text-sm font-medium epaper:border-black" @click="keyHidden = false">
      <KeyRound :size="14" aria-hidden="true" />Save the recovery key
    </button>
    <Btn variant="ghost" :disabled="sync.busy" @click="openForm({ kind: 'create' })"><Plus :size="16" />New vault</Btn>

    <!-- one-time recovery key -->
    <Modal v-if="sync.vaultRecoveryCode && !keyHidden" title="Save your recovery key" @close="keyHidden = true">
      <div class="space-y-3">
        <div class="grid grid-cols-3 gap-1.5 font-mono text-base font-semibold tracking-wider sm:grid-cols-4" aria-label="Recovery key">
          <span v-for="(g, i) in groups" :key="i" class="rounded-md bg-surface-2 px-2 py-1 text-center epaper:border">{{ g }}</span>
        </div>
        <p class="text-sm leading-relaxed">If you forget the passphrase, this key is the only way back in. Penroam can't reset either one. Store it in a password manager or write it down; it's shown only until you confirm.</p>
        <Btn variant="outline" @click="copyCode"><Copy :size="16" />Copy</Btn>
        <label class="block text-sm">
          To confirm you saved it, type the last group ({{ groups.length }} of {{ groups.length }}):
          <input v-model="confirmGroup" autofocus class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-3 font-mono text-base uppercase outline-none focus:border-brand epaper:border-2" autocomplete="off" spellcheck="false" />
        </label>
        <div class="flex justify-end">
          <Btn variant="primary" :disabled="!confirmed" @click="saved">I saved the key</Btn>
        </div>
      </div>
    </Modal>

    <Modal v-if="form" :title="form.kind === 'create' ? 'New vault' : form.kind === 'unlock' ? 'Unlock vault' : 'Change passphrase'" @close="form = null">
      <form class="space-y-3" @submit.prevent="canSubmit && submit()">
        <p v-if="form.kind === 'create'" class="text-sm leading-relaxed text-muted">Notebooks in a vault are readable only with its passphrase. Penroam can't read them or reset the passphrase. You enter it once on each device.</p>
        <p v-else-if="form.kind === 'unlock'" class="text-sm leading-relaxed text-muted">Enter the passphrase once; this device remembers the vault after that.</p>
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
  </section>
</template>
