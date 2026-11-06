<script setup lang="ts">
// Settings → Backup: one file holding every notebook, and restoring from it. A restore shows
// what is in the file first, and never replaces a notebook that is already here.
import { computed, onMounted, ref } from "vue";
import { CloudUpload, DatabaseBackup, FolderOpen } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import Modal from "./ui/Modal.vue";
import type { BackupPreview } from "../generated/BackupPreview";
import type { ConflictPolicy } from "../generated/ConflictPolicy";
import { api } from "../platform/ipc";
import type { RemoteFile } from "../generated/RemoteFile";
import { autoTarget, runAutoBackup } from "../platform/backup";
import { isMobile } from "../platform/device";
import { pickFile, pickFolder, pickSaveLocation } from "../platform/files";
import { flushEverything } from "../platform/lifecycle";
import { formatBytes } from "../platform/search";
import { useLibrary } from "../stores/library";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const lib = useLibrary();
const ui = useUi();
const settings = useSettings();
const auto = computed(() => settings.s.autoBackup);
const path = ref("");
/** `path` is a download of ours, to delete afterwards. */
const fetched = ref(false);
const remote = ref<RemoteFile[]>([]);
const password = ref("");
const autoBusy = ref(false);
const preview = ref<BackupPreview | null>(null);
const copies = ref(false);

const fresh = computed(() => preview.value?.notebooks.filter((n) => !n.alreadyInLibrary).length ?? 0);
const existing = computed(() => preview.value?.notebooks.filter((n) => n.alreadyInLibrary).length ?? 0);
const restoreCount = computed(() => fresh.value + (copies.value ? existing.value : 0));

const when = (ms: number) => new Date(ms).toLocaleString();
const lastBackup = computed(() => (auto.value.lastAt ? `Last backup: ${when(auto.value.lastAt)}.` : "No automatic backup yet."));

function setAuto(patch: Partial<typeof settings.s.autoBackup>) {
  settings.s.autoBackup = { ...settings.s.autoBackup, ...patch };
}

async function withBusy<T>(label: string, what: () => Promise<T>): Promise<T> {
  lib.busy = label;
  lib.progress = { label, fraction: null };
  try {
    return await what();
  } finally {
    lib.busy = null;
    lib.progress = null;
  }
}

async function backUp() {
  if (!(await lib.confirmPlaintext())) return;
  const stamp = new Date().toISOString().slice(0, 10);
  const dest = await pickSaveLocation("backup", `Penroam backup ${stamp}`);
  if (!dest) return;
  try {
    const r = await withBusy("Starting the backup", async () => {
      await flushEverything();
      return api.exportBackup(dest);
    });
    ui.success(`Backed up ${r.notebooks} notebook${r.notebooks === 1 ? "" : "s"} (${formatBytes(r.bytes)}). The file was read back and checked.`);
    for (const u of r.unreadable) ui.toast("error", `“${u.title}” could not be read and is not in this backup: ${u.error}`, { sticky: true });
  } catch (e: any) {
    ui.error(`Backup failed: ${e?.message ?? e}`);
  }
}

async function showPreview(p: string) {
  path.value = p;
  copies.value = false;
  preview.value = await withBusy("Checking the backup", () => api.previewBackup(p));
}

async function open() {
  const f = await pickFile("backup");
  if (!f) return;
  fetched.value = false;
  try {
    await showPreview(f.path);
  } catch (e: any) {
    ui.error(`This backup can't be used: ${e?.message ?? e}`);
  }
}

async function discard() {
  if (fetched.value) await api.discardFetched(path.value).catch(() => {});
  fetched.value = false;
}

async function closePreview() {
  preview.value = null;
  await discard();
}

async function restore() {
  const onConflict: ConflictPolicy = copies.value ? "copy" : "skip";
  preview.value = null;
  try {
    const r = await withBusy("Restoring", () => api.restoreBackup(path.value, onConflict));
    await lib.refresh();
    ui.success(`Restored ${r.restored.length} notebook${r.restored.length === 1 ? "" : "s"}. Search is catching up in the background.`);
    for (const f of r.failed) ui.toast("error", `“${f.title}” could not be restored: ${f.error}`, { sticky: true });
  } catch (e: any) {
    ui.error(`Restore failed: ${e?.message ?? e}`);
  } finally {
    await discard();
  }
}

async function chooseFolder() {
  const dir = await pickFolder();
  // a new destination holds nothing yet, so its first backup is due straight away
  if (dir) setAuto({ target: "folder", dir, lastAt: 0 });
}

async function connectWebdav() {
  try {
    remote.value = await api.webdavConnect(auto.value.url, auto.value.user, password.value);
    password.value = "";
    setAuto({ target: "webdav", lastAt: 0 });
    ui.success("Connected. Backups will go to this folder.");
  } catch (e: any) {
    ui.error(`Could not connect: ${e?.message ?? e}`);
  }
}

async function disconnectWebdav() {
  try {
    await api.webdavDisconnect();
    remote.value = [];
    setAuto({ target: "off" });
  } catch (e: any) {
    ui.error(`Could not disconnect: ${e?.message ?? e}`);
  }
}

async function loadRemote() {
  if (auto.value.target !== "webdav") return;
  remote.value = await api.webdavBackups(auto.value.url, auto.value.user).catch(() => []);
}
onMounted(loadRemote);

async function backUpNow() {
  autoBusy.value = true;
  try {
    await runAutoBackup();
    ui.success("Backed up.");
    await loadRemote();
  } catch (e: any) {
    ui.error(`Backup failed: ${e?.message ?? e}`);
  } finally {
    autoBusy.value = false;
  }
}

async function restoreRemote(name: string) {
  try {
    const p = await withBusy("Downloading the backup", () => api.webdavFetch(auto.value.url, auto.value.user, name));
    fetched.value = true;
    await showPreview(p);
  } catch (e: any) {
    ui.error(`This backup can't be used: ${e?.message ?? e}`);
    await discard();
  }
}
</script>

<template>
  <div class="space-y-6">
    <section>
      <h3 class="mb-1 text-sm font-semibold">Back up this library</h3>
      <p class="mb-3 text-xs leading-relaxed text-muted">
        Saves every notebook, including the trash, to one file that any Penroam can restore. It needs no account and works offline.
        The file is not encrypted, so anyone who gets it can read your notes. Keep it somewhere safe.
        Version history isn't included.
      </p>
      <Btn variant="outline" @click="backUp"><DatabaseBackup :size="18" />Back up…</Btn>
    </section>
    <section>
      <h3 class="mb-1 text-sm font-semibold">Restore from a backup</h3>
      <p class="mb-3 text-xs leading-relaxed text-muted">You'll see what's in the file before anything changes. Notebooks you already have are left as they are.</p>
      <Btn variant="outline" @click="open"><FolderOpen :size="18" />Choose a backup…</Btn>
    </section>
    <section>
      <h3 class="mb-1 text-sm font-semibold">Automatic backups</h3>
      <p class="mb-3 text-xs leading-relaxed text-muted">
        Once a day while Penroam is open, keeping the last seven. The files are whole backups and aren't encrypted. Storage like this is for safekeeping only: it doesn't sync, and nothing is ever merged.
      </p>
      <div class="flex flex-wrap gap-2">
        <Btn :variant="auto.target === 'off' ? 'primary' : 'outline'" @click="setAuto({ target: 'off' })">Off</Btn>
        <Btn v-if="!isMobile()" :variant="auto.target === 'folder' ? 'primary' : 'outline'" @click="chooseFolder"><FolderOpen :size="18" />Folder…</Btn>
        <Btn :variant="auto.target === 'webdav' ? 'primary' : 'outline'" @click="setAuto({ target: 'webdav' })"><CloudUpload :size="18" />WebDAV</Btn>
      </div>
      <p v-if="auto.target === 'folder'" class="mt-2 break-all text-xs text-muted">{{ auto.dir }}</p>
      <form v-if="auto.target === 'webdav'" class="mt-3 space-y-2" @submit.prevent="connectWebdav">
        <input class="w-full rounded-lg border border-line bg-surface px-3 py-2 text-sm" placeholder="https://cloud.example.com/remote.php/dav/files/me/Penroam" aria-label="WebDAV folder address" inputmode="url" :value="auto.url" @change="setAuto({ url: ($event.target as HTMLInputElement).value })" />
        <input class="w-full rounded-lg border border-line bg-surface px-3 py-2 text-sm" placeholder="User name" aria-label="WebDAV user name" autocomplete="username" :value="auto.user" @change="setAuto({ user: ($event.target as HTMLInputElement).value })" />
        <input v-model="password" class="w-full rounded-lg border border-line bg-surface px-3 py-2 text-sm" type="password" placeholder="Password or app password" aria-label="WebDAV password" autocomplete="current-password" />
        <div class="flex gap-2">
          <Btn type="submit" variant="outline" :disabled="!auto.url || !password">Connect</Btn>
          <Btn @click="disconnectWebdav">Disconnect</Btn>
        </div>
      </form>
      <template v-if="autoTarget(auto)">
        <p class="mt-2 text-xs text-muted">{{ lastBackup }}</p>
        <Btn class="mt-2" variant="outline" :disabled="autoBusy" @click="backUpNow">{{ autoBusy ? "Backing up…" : "Back up now" }}</Btn>
      </template>
      <ul v-if="auto.target === 'webdav' && remote.length" class="mt-3 divide-y divide-line rounded-xl border border-line" aria-label="Backups on the server">
        <li v-for="f in [...remote].reverse()" :key="f.name" class="flex items-center gap-2 px-3 py-2 text-sm">
          <span class="min-w-0 flex-1 truncate">{{ f.name }}</span>
          <Btn @click="restoreRemote(f.name)">Restore…</Btn>
        </li>
      </ul>
    </section>

    <Modal v-if="preview" title="Restore this backup?" wide @close="closePreview">
      <p class="mb-3 text-xs text-muted">Made {{ when(preview.createdAt) }} by {{ preview.generator }}. Every notebook below passed its checks.</p>
      <ul class="max-h-64 divide-y divide-line overflow-y-auto rounded-xl border border-line">
        <li v-for="n in preview.notebooks" :key="n.notebookId" class="flex items-center gap-2 px-3 py-2 text-sm">
          <span class="min-w-0 flex-1 truncate">{{ n.title }}</span>
          <span class="shrink-0 text-xs text-muted">{{ n.pages }} page{{ n.pages === 1 ? "" : "s" }}<template v-if="n.inTrash">, in trash</template></span>
          <span v-if="n.alreadyInLibrary" class="shrink-0 text-xs text-muted">already here</span>
        </li>
      </ul>
      <div v-if="preview.damaged.length || preview.unreadable.length" class="mt-3 rounded-lg border border-danger p-2 text-sm" role="alert">
        <p class="font-medium">These can't be restored:</p>
        <ul class="mt-1 list-inside list-disc text-xs">
          <li v-for="d in preview.damaged" :key="d.id">{{ d.title }}: {{ d.error }}</li>
          <li v-for="d in preview.unreadable" :key="d.id">{{ d.title }}: already unreadable when the backup was made</li>
        </ul>
      </div>
      <label v-if="existing" class="mt-3 flex items-center gap-2 text-sm">
        <input v-model="copies" type="checkbox" />
        Also restore the {{ existing }} already here, as separate copies
      </label>
      <template #actions>
        <Btn autofocus @click="closePreview">Cancel</Btn>
        <Btn variant="primary" :disabled="restoreCount === 0" @click="restore">Restore {{ restoreCount }} notebook{{ restoreCount === 1 ? "" : "s" }}</Btn>
      </template>
    </Modal>
  </div>
</template>
