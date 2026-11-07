<script setup lang="ts">
// Settings → Advanced: lock every vault when the app closes. Shown once the account has vaults.
// The backend also locks at the next launch if the app was stopped without closing, which is
// how phones usually end an app.
import { onMounted, ref } from "vue";
import { useSync } from "../stores/sync";
import { useUi } from "../stores/ui";
import { syncApi } from "../platform/sync";

const sync = useSync();
const ui = useUi();
const lockOnExit = ref<boolean | null>(null);

onMounted(async () => {
  lockOnExit.value = (await syncApi.vaultPrefs().catch(() => null))?.lockOnExit ?? null;
});

async function set(on: boolean) {
  const was = lockOnExit.value;
  lockOnExit.value = on;
  try {
    await syncApi.vaultSetPrefs({ lockOnExit: on });
  } catch (e) {
    lockOnExit.value = was;
    ui.error(`Could not save the setting: ${(e as Error).message ?? e}`);
  }
}
</script>

<template>
  <label v-if="sync.vaults.supported && sync.vaults.vaults.length && lockOnExit !== null" class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
    <span>
      <span class="block text-sm">Lock vaults when the app closes</span>
      <span class="block text-xs text-muted">You'll enter a vault's passphrase each time you open it. If the app is stopped without closing, as phones often do, vaults lock the next time it starts.</span>
    </span>
    <input type="checkbox" class="h-5 w-5" :checked="lockOnExit" @change="set(($event.target as HTMLInputElement).checked)" />
  </label>
</template>
