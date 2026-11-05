<script setup lang="ts">
// Sync (developer preview) settings. Shown only when the backend reports
// sync as available. Notes never depend on sync: everything here can be
// disconnected without touching local notebooks.
import { computed, onMounted, ref } from "vue";
import { Cloud, Copy, KeyRound, Link2, LogIn, Pause, Play, RefreshCw, ShieldAlert, Smartphone, Trash2, Unplug } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import CloudAccountSection from "./CloudAccountSection.vue";
import { useSync } from "../stores/sync";
import { useUi } from "../stores/ui";
import { codeGroups, describeStatus, formatAgo, type SyncDevice } from "../platform/sync";

const sync = useSync();
const ui = useUi();
const serverUrl = ref("http://127.0.0.1:8787");
const account = ref("");
const label = ref(defaultLabel());
const mode = ref<"choose" | "recover">("choose");
const recoveryInput = ref("");
const confirmGroup = ref("");
const approveCode = ref("");
const approveLabel = ref("");
const approvedSas = ref<string | null>(null);
const removing = ref<SyncDevice | null>(null);
const disconnecting = ref(false);

function defaultLabel(): string {
  const ua = navigator.userAgent;
  if (/Android/i.test(ua)) return "Android tablet";
  if (/iPad|iPhone/i.test(ua)) return "iPad";
  if (/Mac/i.test(ua)) return "Mac";
  if (/Windows/i.test(ua)) return "Windows PC";
  return "Linux computer";
}

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
const groups = computed(() => codeGroups(sync.recoveryCode ?? ""));
const lastGroup = computed(() => groups.value[groups.value.length - 1] ?? "");
const confirmed = computed(() => confirmGroup.value.trim().toUpperCase() === lastGroup.value.toUpperCase() && lastGroup.value !== "");
/** The checked server is ready for setup: signed in, or a test server with an account name. */
const ready = computed(() => {
  const srv = sync.server;
  if (!srv) return false;
  return srv.signIn ? srv.signedInAccount !== null : account.value.trim() !== "";
});
const canSetup = computed(() => ready.value && label.value.trim() !== "" && !sync.busy);
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

async function approve() {
  approvedSas.value = (await sync.approve(approveCode.value.trim(), approveLabel.value.trim() || "New device")) ?? null;
  if (approvedSas.value) {
    approveCode.value = "";
    void sync.refresh();
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
      <span class="rounded-md bg-surface-2 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-muted epaper:border">developer preview</span>
    </h3>
    <p class="text-xs leading-relaxed text-muted">
      <template v-if="sync.customServers">Developer mode: works with any Penroam sync server (you sign in through your web browser) or a local development test server.</template>
      <template v-else>Syncs through Penroam Cloud; you sign in through your web browser.</template>
      Notes are encrypted on this device before upload; the server stores ciphertext.
    </p>
    <p v-if="sync.error" class="rounded-lg border border-danger/40 p-2 text-xs text-danger" role="alert">{{ sync.error }}</p>

    <!-- one-time recovery code -->
    <div v-if="sync.step === 'recovery'" class="space-y-3 rounded-xl border-2 border-warn p-3 epaper:border-black">
      <p class="flex items-center gap-2 text-sm font-semibold"><KeyRound :size="16" />Save your recovery code</p>
      <div class="grid grid-cols-3 gap-1.5 font-mono text-base font-semibold tracking-wider sm:grid-cols-4" aria-label="Recovery code">
        <span v-for="(g, i) in groups" :key="i" class="rounded-md bg-surface-2 px-2 py-1 text-center epaper:border">{{ g }}</span>
      </div>
      <p class="text-xs leading-relaxed">
        This code is the only way to read your synced notes if you lose this device and have no other approved device.
        Penroam cannot reset it or recover it for you. Write it down or store it in a password manager; it is shown only once.
      </p>
      <Btn variant="outline" @click="copy(sync.recoveryCode ?? '', 'Recovery code')"><Copy :size="16" />Copy</Btn>
      <label class="block text-xs">
        To confirm you saved it, type the last group ({{ groups.length }} of {{ groups.length }}):
        <input v-model="confirmGroup" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 font-mono uppercase" autocomplete="off" spellcheck="false" />
      </label>
      <Btn variant="primary" :disabled="!confirmed" @click="sync.finishRecovery()">I saved the code</Btn>
    </div>

    <!-- new device waiting for approval -->
    <div v-else-if="sync.step === 'pairing'" class="space-y-2 rounded-xl border border-line p-3">
      <p class="text-sm font-medium">On a device that already syncs, open Settings → Sync → Approve a device and enter:</p>
      <p class="select-all text-center font-mono text-xl font-semibold tracking-wider" aria-label="Pairing code">{{ sync.pairingCode }}</p>
      <p class="flex items-center gap-2 text-xs text-muted"><RefreshCw :size="14" class="animate-spin" />Waiting for approval…</p>
    </div>

    <div v-else-if="sync.step === 'compare'" class="space-y-3 rounded-xl border-2 border-brand p-3 epaper:border-black">
      <p class="text-sm font-medium">Check that the other device shows the same code:</p>
      <p class="text-center font-mono text-3xl font-bold tracking-widest">{{ sync.comparison }}</p>
      <div class="flex flex-wrap gap-2">
        <Btn variant="primary" @click="sync.confirmComparison(true)">They match</Btn>
        <Btn variant="danger" @click="sync.confirmComparison(false)">They differ</Btn>
      </div>
    </div>

    <!-- not set up: 1. server, 2. sign in (or account name on a test server), 3. set up -->
    <div v-else-if="!sync.configured" class="space-y-2">
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
        <template v-if="ready">
          <label class="block text-xs">This device's name (only your devices see it)
            <input v-model="label" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 text-sm" autocomplete="off" />
          </label>
          <template v-if="mode === 'choose'">
            <div class="flex flex-col gap-2 pt-1">
              <Btn variant="primary" :disabled="!canSetup" @click="sync.setupFirst(setupUrl, setupAccount, label)"><Cloud :size="16" />Set up on this first device</Btn>
              <Btn variant="outline" :disabled="!canSetup" @click="sync.setupPair(setupUrl, setupAccount, label)"><Link2 :size="16" />Add this device (pair with a trusted device)</Btn>
              <Btn variant="ghost" :disabled="!canSetup" @click="mode = 'recover'"><KeyRound :size="16" />Use recovery code</Btn>
            </div>
          </template>
          <template v-else>
            <label class="block text-xs">Recovery code
              <input v-model="recoveryInput" class="mt-1 w-full rounded-lg border border-line bg-surface px-2 py-1.5 font-mono text-sm uppercase" autocomplete="off" spellcheck="false" placeholder="PRK1-…" />
            </label>
            <div class="flex gap-2">
              <Btn variant="primary" :disabled="!canSetup || !recoveryInput.trim()" @click="sync.setupRecovery(setupUrl, setupAccount, label, recoveryInput)">Add this device</Btn>
              <Btn @click="mode = 'choose'">Back</Btn>
            </div>
          </template>
          <p class="text-xs text-muted">Notebooks already on this device are uploaded (encrypted) once sync is set up.</p>
        </template>
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
        <div v-if="sync.awaiting && sync.step === 'none'" class="mt-2">
          <Btn variant="outline" @click="sync.step = 'pairing'; sync.pollPairing()">Check for approval</Btn>
        </div>
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
          End-to-end encryption has not been independently reviewed yet.
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
            <button v-if="d.active && !d.thisDevice && sync.status?.phase !== 'revoked'" class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-label="`Remove ${d.label}`" @click="removing = d">
              <Trash2 :size="16" />
            </button>
          </li>
        </ul>
      </div>

      <div v-if="sync.status?.phase !== 'revoked'" class="space-y-2 rounded-xl border border-line p-3">
        <h4 class="text-xs font-semibold uppercase tracking-wide text-muted">Approve a device</h4>
        <p class="text-xs text-muted">Enter the pairing code shown on the new device.</p>
        <input v-model="approveCode" class="w-full rounded-lg border border-line bg-surface px-2 py-1.5 font-mono text-sm uppercase" placeholder="XXXXXXXX-XXXX-XXXX-XXXX-XXXX" autocomplete="off" spellcheck="false" aria-label="Pairing code" />
        <input v-model="approveLabel" class="w-full rounded-lg border border-line bg-surface px-2 py-1.5 text-sm" placeholder="Name for the new device" autocomplete="off" aria-label="Name for the new device" />
        <Btn variant="outline" :disabled="!approveCode.trim() || sync.busy" @click="approve">Approve</Btn>
        <div v-if="approvedSas" class="rounded-lg bg-surface-2 p-2 text-sm epaper:border">
          Approved. The new device must now show <span class="font-mono text-lg font-bold">{{ approvedSas }}</span>.
          If it shows a different code, remove that device right away.
        </div>
      </div>

      <div class="flex flex-wrap gap-2">
        <Btn variant="ghost" @click="copyDiagnostics"><Copy :size="16" />Copy diagnostics</Btn>
        <Btn variant="ghost" @click="disconnecting = true"><Unplug :size="16" />Disconnect</Btn>
      </div>
    </div>

    <ConfirmDialog v-if="removing" title="Remove this device?" danger confirm="Remove device"
      :message="`“${removing.label}” will no longer receive new changes: a new encryption key epoch starts for the remaining devices. It keeps whatever it already downloaded, and its notes on that device are not deleted.`"
      @close="removing = null" @confirm="confirmRemove" />
    <ConfirmDialog v-if="disconnecting" title="Disconnect sync?" confirm="Disconnect"
      message="All notebooks stay on this device. Sync keys and sync history on this device are removed; changes not yet uploaded stay here and will not be uploaded to another account automatically. You can set up sync again later."
      @close="disconnecting = false" @confirm="confirmDisconnect" />
  </section>
</template>
