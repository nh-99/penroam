<script setup lang="ts">
// Sync settings. Shown only when the backend reports sync as available.
// Notes never depend on sync: everything here can be
// disconnected without touching local notebooks.
import { computed, onMounted, ref } from "vue";
import { Cloud, Copy, KeyRound, LogIn, Pause, Pencil, Play, RefreshCw, ShieldAlert, Smartphone, Trash2, Unplug } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import PromptDialog from "./PromptDialog.vue";
import CloudAccountSection from "./CloudAccountSection.vue";
import { useSync } from "../stores/sync";
import { useUi } from "../stores/ui";
import { describeStatus, formatAgo, type SyncDevice } from "../platform/sync";

const sync = useSync();
const ui = useUi();
const serverUrl = ref("http://127.0.0.1:8787");
const account = ref("");
const recoveryInput = ref("");
const removing = ref<SyncDevice | null>(null);
const renaming = ref<SyncDevice | null>(null);
const disconnecting = ref(false);

const desc = computed(() => describeStatus(sync.status));
const toneClass = computed(
  () =>
    ({
      ok: "text-ok",
      busy: "text-ink",
      muted: "text-muted",
      warn: "text-warn",
      danger: "text-danger",
    })[desc.value.tone],
);
/** Signed in, or a test server with an account name; signing in normally connects by itself. */
const canConnect = computed(() => {
  const srv = sync.server;
  if (!srv || sync.needsRecoveryCode) return false;
  return srv.signIn ? srv.signedInAccount !== null : account.value.trim() !== "";
});
/** Account argument of the setup commands: the server's own for sign-in servers. */
const setupAccount = computed(() => (sync.server?.signIn ? "" : account.value.trim()));
const setupUrl = computed(() => sync.server?.url ?? serverUrl.value.trim());

onMounted(() => void sync.refresh());

async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    ui.success(`${what} copied.`);
  } catch {
    ui.error("Copying is not available here; select the text instead.");
  }
}

async function copyDiagnostics() {
  const d = await sync.diagnostics();
  if (d) await copy(d, "Diagnostics");
}

async function confirmRemove() {
  const d = removing.value;
  removing.value = null;
  if (d) await sync.remove(d.deviceId);
}

async function confirmDisconnect() {
  disconnecting.value = false;
  await sync.disconnect();
}
</script>

<template>
  <section class="space-y-3" aria-labelledby="sync-h">
    <h3 id="sync-h" class="flex items-center gap-2 text-sm font-medium">
      <Cloud :size="16" />Sync
    </h3>
    <p class="text-xs leading-relaxed text-muted">
      <template v-if="sync.customServers">Developer mode: works with any Penroam sync server (you sign in through your web browser) or a local development test server.</template>
      <template v-else>Syncs through Penroam Cloud; you sign in through your web browser.</template>
      Notes are encrypted before upload, and Penroam keeps a copy of the key so that signing in is all a new device needs. For notes only you can read, use a vault.
    </p>
    <p v-if="sync.error" class="rounded-lg border border-danger/40 p-2 text-xs text-danger" role="alert">{{ sync.error }}</p>

    <!-- not set up: 1. server, 2. sign in (or an account name on a test server); that's all -->
    <div v-if="!sync.configured" class="space-y-2">
      <template v-if="!sync.server">
        <template v-if="sync.customServers">
          <label class="block text-xs">Server URL (developer mode)
            <input v-model="serverUrl" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 text-sm" inputmode="url" autocomplete="off" @keydown.enter="sync.checkServer(serverUrl.trim())" />
          </label>
          <Btn variant="primary" :disabled="!serverUrl.trim() || sync.busy" @click="sync.checkServer(serverUrl.trim())">Continue</Btn>
        </template>
        <template v-else>
          <Btn variant="primary" :disabled="sync.busy" @click="sync.checkServer(sync.defaultServerUrl)">Continue</Btn>
        </template>
      </template>
      <template v-else>
        <!-- the server address is an implementation detail; only developer mode shows it -->
        <p v-if="sync.customServers" class="flex items-center gap-2 text-xs">
          <span class="min-w-0 flex-1 truncate">Server: <span class="font-mono">{{ sync.server.url }}</span></span>
          <button class="touch-target rounded-lg px-2 text-brand underline-offset-2 hover:underline" :disabled="sync.busy" @click="sync.changeServer()">Change</button>
        </p>
        <template v-if="sync.server.signIn">
          <div v-if="!sync.server.signedInAccount" class="space-y-2">
            <p class="text-xs leading-relaxed text-muted">This server signs you in through your web browser. Penroam never sees your password.</p>
            <Btn variant="primary" :disabled="sync.busy" @click="sync.signIn(sync.server.url)"><LogIn :size="16" />Sign in with your browser</Btn>
            <p v-if="sync.signingIn" class="flex items-center gap-2 text-xs text-muted" role="status"><RefreshCw :size="14" class="animate-spin" />Finish signing in in your browser, then come back here.</p>
          </div>
          <p v-else class="flex items-center gap-2 text-xs">
            <span class="flex-1">Signed in.</span>
            <button class="touch-target rounded-lg px-2 text-brand underline-offset-2 hover:underline" :disabled="sync.busy" @click="sync.signOut(sync.server.url)">Sign out</button>
          </p>
        </template>
        <label v-else class="block text-xs">Account name (development test server)
          <input v-model="account" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 text-sm" autocomplete="off" placeholder="e.g. my-notes" />
        </label>
        <Btn v-if="canConnect" variant="primary" :disabled="sync.busy" @click="sync.connect(setupUrl, setupAccount)"><Cloud :size="16" />Connect</Btn>
        <template v-if="sync.needsRecoveryCode">
          <label class="block text-xs">This account was set up before sign-in-only sync. Enter its recovery code once; after that, signing in is enough.
            <input v-model="recoveryInput" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 font-mono text-sm uppercase" autocomplete="off" spellcheck="false" placeholder="PRK1-…" />
          </label>
          <Btn variant="primary" :disabled="!recoveryInput.trim() || sync.busy" @click="sync.connect(setupUrl, setupAccount, recoveryInput)"><KeyRound :size="16" />Connect</Btn>
        </template>
        <p class="text-xs text-muted">Notebooks already on this device are uploaded once sync is connected. You can rename this device afterwards.</p>
      </template>
    </div>

    <!-- configured -->
    <div v-else class="space-y-3">
      <div class="rounded-xl border border-line p-3" role="status" aria-live="polite">
        <p class="text-sm font-medium" :class="toneClass">{{ desc.text }}</p>
        <p class="mt-0.5 text-xs text-muted">
          Last successful sync: {{ formatAgo(sync.status?.lastSuccessMs ?? null) }}
          <template v-if="sync.status?.conflicts"> · {{ sync.status.conflicts }} conflict(s) to review</template>
          <template v-if="sync.status?.quarantined"> · {{ sync.status.quarantined }} item(s) set aside</template>
        </p>
        <div v-if="sync.status?.phase === 'needsReauth'" class="mt-2 space-y-1">
          <Btn variant="primary" :disabled="sync.busy" @click="sync.signIn(sync.status.serverUrl)"><LogIn :size="16" />Sign in again</Btn>
          <p v-if="sync.signingIn" class="flex items-center gap-2 text-xs text-muted" role="status"><RefreshCw :size="14" class="animate-spin" />Finish signing in in your browser, then come back here.</p>
        </div>
        <p v-if="sync.status?.phase === 'revoked'" class="mt-2 text-xs text-muted">
          Disconnect below to stop syncing on this device. Your notes stay here; you can set up sync again later.
        </p>
        <div v-else class="mt-2 flex flex-wrap gap-2">
          <Btn variant="outline" :disabled="sync.busy" @click="sync.syncNow()"><RefreshCw :size="16" />Sync now</Btn>
          <Btn v-if="sync.status?.phase === 'paused'" variant="outline" @click="sync.pause(false)"><Play :size="16" />Resume</Btn>
          <Btn v-else variant="outline" @click="sync.pause(true)"><Pause :size="16" />Pause</Btn>
        </div>
      </div>

      <div v-if="sync.status?.securityReviewPending" class="flex gap-2 rounded-xl bg-surface-2 p-3 text-xs leading-relaxed epaper:border">
        <ShieldAlert :size="16" class="shrink-0 text-warn" />
        <p>
          Vault encryption has not been independently reviewed yet.
          <template v-if="sync.status.secretStore === 'file'">This device's keys are in a file only your user account can read, because no system keychain was available (development override).</template>
          <template v-else>This device's keys are stored in the {{ sync.status.secretStore }}.</template>
        </p>
      </div>

      <CloudAccountSection v-if="sync.status?.phase !== 'revoked'" />

      <div>
        <h4 class="mb-1.5 text-xs font-semibold uppercase tracking-wide text-muted">Devices</h4>
        <ul class="space-y-1.5">
          <li v-for="d in sync.devices" :key="d.deviceId" class="flex items-center gap-2 rounded-xl border border-line p-2.5" :class="d.active ? '' : 'opacity-60'">
            <Smartphone :size="16" class="shrink-0 text-muted" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm">{{ d.label || "Unnamed device" }}<span v-if="d.thisDevice" class="text-muted"> (this device)</span><span v-if="!d.active" class="text-muted"> · removed</span></p>
              <p class="font-mono text-[11px] text-muted">{{ d.fingerprint }}</p>
            </div>
            <button v-if="d.active && sync.status?.phase !== 'revoked'" class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-label="`Rename ${d.label}`" @click="renaming = d">
              <Pencil :size="16" />
            </button>
            <button v-if="d.active && !d.thisDevice && sync.status?.phase !== 'revoked'" class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-label="`Remove ${d.label}`" @click="removing = d">
              <Trash2 :size="16" />
            </button>
          </li>
        </ul>
      </div>

      <p v-if="sync.vaults.supported && sync.status?.phase !== 'revoked'" class="text-xs leading-relaxed text-muted">Vaults are on the library screen, above your notebooks.</p>

      <div class="flex flex-wrap gap-2">
        <Btn variant="ghost" @click="copyDiagnostics"><Copy :size="16" />Copy diagnostics</Btn>
        <Btn variant="ghost" @click="disconnecting = true"><Unplug :size="16" />Disconnect</Btn>
      </div>
    </div>

    <PromptDialog v-if="renaming" title="Rename device" label="Update Penroam on your other devices first: older versions stop syncing after a rename until they are updated. Only your devices show this name, but it isn't encrypted, so keep it generic."
      :value="renaming.label" confirm="Rename" @close="renaming = null"
      @submit="(v: string) => { const d = renaming; renaming = null; if (d) sync.rename(d.deviceId, v); }" />
    <ConfirmDialog v-if="removing" title="Remove this device?" danger confirm="Remove device"
      :message="`“${removing.label}” will no longer receive new changes: it has to sign in again to come back. It keeps whatever it already downloaded, and its notes on that device are not deleted.`"
      @close="removing = null" @confirm="confirmRemove" />
    <ConfirmDialog v-if="disconnecting" title="Disconnect sync?" confirm="Disconnect"
      message="All notebooks stay on this device. Sync keys and sync history on this device are removed; changes not yet uploaded stay here and will not be uploaded to another account automatically. You can set up sync again later."
      @close="disconnecting = false" @confirm="confirmDisconnect" />
  </section>
</template>
