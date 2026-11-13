<script setup lang="ts">
// Share a notebook: who has access, what each person may do, invitations, and what sharing means
// for privacy. Rust holds the truth (shared_members); this reloads it after every change.
// Presented by Modal, so it is a bottom sheet on narrow screens, traps focus, and closes on Escape.
import { computed, onMounted, ref } from "vue";
import { LoaderCircle, Mail, UserMinus, X, ShieldAlert } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { cleanName } from "../platform/display";
import ConfirmDialog from "./ConfirmDialog.vue";
import { useSync } from "../stores/sync";
import { useSharing } from "../stores/sharing";
import { useUi } from "../stores/ui";
import {
  describeSharingError,
  memberName,
  roleLabel,
  shareBlockedReason,
  sharingApi,
  SHARING_PRIVACY_POINTS,
  type InviteRole,
  type InvitationView,
  type MemberView,
  type MembersView,
  type SharedNotebookView,
} from "../platform/sharing";

const props = defineProps<{ notebookId: string; title: string }>();
const emit = defineEmits<{ close: [] }>();
const sync = useSync();
const sharing = useSharing();
const ui = useUi();

const blocked = computed(() =>
  shareBlockedReason({ inVault: !!sync.vaults.notebooks[props.notebookId], configured: sync.configured, available: sharing.available }),
);
const entry = computed<SharedNotebookView | undefined>(() => sharing.byNotebook.get(props.notebookId));
const people = ref<MembersView | null>(null);
const loading = ref(false);
const busy = ref(false);
const error = ref<string | null>(null);
const email = ref("");
const inviteRole = ref<InviteRole>("editor");
const removing = ref<MemberView | null>(null);
const leaving = ref(false);

const isOwner = computed(() => people.value?.you === "owner" || (!people.value && !!entry.value?.owner));
const pending = computed(() => (people.value?.invitations ?? []).filter((i) => i.state === "pending"));
// former members stay in the list, without an email, so it is clear who had access
const members = computed(() => people.value?.members ?? []);

async function load() {
  const e = entry.value;
  if (!e) return;
  loading.value = true;
  try {
    people.value = await sharingApi.members(e.spaceId);
    error.value = null;
  } catch (err) {
    error.value = describeSharingError(err);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

/** Runs one change, then reloads the list; the message says what failed. */
async function act(what: string, fn: () => Promise<unknown>): Promise<boolean> {
  busy.value = true;
  error.value = null;
  try {
    await fn();
    return true;
  } catch (err) {
    error.value = `${what}: ${describeSharingError(err)}`;
    return false;
  } finally {
    busy.value = false;
  }
}

async function start() {
  if (await act("Could not share the notebook", () => sharingApi.share(props.notebookId))) {
    await sharing.refresh();
    await load();
  }
}
async function invite() {
  const e = entry.value;
  const addr = email.value.trim();
  if (!e || !addr) return;
  if (await act("Could not send the invitation", () => sharingApi.invite(e.spaceId, addr, inviteRole.value))) {
    email.value = "";
    ui.success(`Invitation sent to ${addr}.`);
    await load();
  }
}
async function cancel(i: InvitationView) {
  const e = entry.value;
  if (e && (await act("Could not cancel the invitation", () => sharingApi.cancelInvitation(e.spaceId, i.invitationId)))) await load();
}
async function changeRole(m: MemberView, role: InviteRole) {
  const e = entry.value;
  if (!e || role === m.role) return;
  const ok = await act("Could not change the role", () => sharingApi.changeRole(e.spaceId, m.accountId, role, m.version));
  await load();
  return ok;
}
async function confirmRemove() {
  const e = entry.value;
  const m = removing.value;
  removing.value = null;
  if (e && m && (await act("Could not remove the person", () => sharingApi.removeMember(e.spaceId, m.accountId)))) await load();
}
async function confirmLeave() {
  const e = entry.value;
  leaving.value = false;
  if (e && (await act("Could not leave the notebook", () => sharingApi.leave(e.spaceId)))) {
    await sharing.refresh();
    emit("close");
  }
}
const asInviteRole = (e: Event) => (e.target as HTMLSelectElement).value as InviteRole;
</script>

<template>
  <Modal :title="`Share “${title}”`" wide @close="emit('close')">
    <div v-if="blocked" class="flex items-start gap-3 rounded-xl border border-line bg-surface-2 p-3 text-sm" role="note">
      <ShieldAlert :size="18" class="mt-0.5 shrink-0 text-muted" aria-hidden="true" />
      <p>{{ blocked }}</p>
    </div>

    <template v-else>
      <section aria-labelledby="share-privacy" class="rounded-xl border border-line p-3">
        <h3 id="share-privacy" class="text-sm font-semibold">Before you share</h3>
        <ul class="mt-2 list-disc space-y-1 pl-5 text-xs leading-relaxed text-muted">
          <li v-for="p in SHARING_PRIVACY_POINTS" :key="p">{{ p }}</li>
        </ul>
      </section>

      <p v-if="error" class="mt-3 text-sm text-danger" role="alert">{{ error }}</p>

      <div v-if="!entry" class="mt-4">
        <p class="text-sm leading-relaxed">This notebook is only yours so far. Start sharing to invite people by email.</p>
        <Btn class="mt-3" variant="primary" :disabled="busy" @click="start">
          <LoaderCircle v-if="busy" :size="16" class="animate-spin" aria-hidden="true" />Start sharing
        </Btn>
      </div>

      <template v-else>
        <p v-if="loading && !people" class="mt-4 text-sm text-muted" role="status">Loading…</p>

        <section v-if="people" class="mt-4" aria-labelledby="share-people">
          <h3 id="share-people" class="text-sm font-semibold">People with access</h3>
          <ul class="mt-2 divide-y divide-line rounded-xl border border-line">
            <li v-for="m in members" :key="m.accountId" class="flex flex-wrap items-center gap-2 px-3 py-2">
              <span class="min-w-0 flex-1 truncate text-sm"><bdi>{{ memberName(m) }}</bdi><span v-if="m.isYou" class="text-muted"> (you)</span></span>
              <template v-if="isOwner && m.role !== 'owner' && !m.isYou && m.state !== 'revoked'">
                <label class="sr-only" :for="`role-${m.accountId}`">Role for {{ memberName(m) }}</label>
                <select :id="`role-${m.accountId}`" class="h-11 rounded-xl border border-line bg-surface px-2 text-sm epaper:border-2" :value="m.role" :disabled="busy" @change="changeRole(m, asInviteRole($event))">
                  <option value="editor">Can edit</option>
                  <option value="commenter">Can comment</option>
                  <option value="viewer">View only</option>
                </select>
                <button type="button" class="touch-target inline-flex items-center justify-center rounded-lg text-danger hover:bg-surface-2" :aria-label="`Remove ${memberName(m)}`" :disabled="busy" @click="removing = m">
                  <UserMinus :size="18" />
                </button>
              </template>
              <span v-else class="rounded-full border border-line px-2 py-0.5 text-xs text-muted epaper:border-black epaper:text-black">{{ m.state === 'revoked' ? 'Access removed' : roleLabel(m.role) }}</span>
            </li>
          </ul>
        </section>

        <section v-if="isOwner && people && pending.length" class="mt-4" aria-labelledby="share-pending">
          <h3 id="share-pending" class="text-sm font-semibold">Waiting to accept</h3>
          <ul class="mt-2 divide-y divide-line rounded-xl border border-line">
            <li v-for="i in pending" :key="i.invitationId" class="flex flex-wrap items-center gap-2 px-3 py-2">
              <Mail :size="16" class="shrink-0 text-muted" aria-hidden="true" />
              <span class="min-w-0 flex-1 truncate text-sm"><bdi>{{ cleanName(i.email) }}</bdi></span>
              <span class="rounded-full border border-line px-2 py-0.5 text-xs text-muted epaper:border-black epaper:text-black">{{ roleLabel(i.role) }}</span>
              <button type="button" class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-label="`Cancel invitation to ${cleanName(i.email)}`" :disabled="busy" @click="cancel(i)">
                <X :size="18" />
              </button>
            </li>
          </ul>
        </section>

        <form v-if="isOwner && people" class="mt-4 space-y-2" aria-labelledby="share-invite" @submit.prevent="invite">
          <h3 id="share-invite" class="text-sm font-semibold">Invite someone</h3>
          <p class="text-xs text-muted">They need a Penroam account with this verified email address. You can't see whether an address has one.</p>
          <div class="flex flex-wrap gap-2">
            <label class="min-w-0 flex-1 basis-56 text-sm">
              <span class="sr-only">Email address</span>
              <input v-model="email" type="email" required autocomplete="off" inputmode="email" placeholder="name@example.com" class="h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" />
            </label>
            <label class="text-sm">
              <span class="sr-only">Role</span>
              <select v-model="inviteRole" class="h-11 rounded-xl border border-line bg-surface px-2 text-sm epaper:border-2">
                <option value="editor">Can edit</option>
                <option value="commenter">Can comment</option>
                <option value="viewer">View only</option>
              </select>
            </label>
            <Btn variant="primary" type="submit" :disabled="busy || !email.trim()">Send invitation</Btn>
          </div>
        </form>

        <p v-if="people && !isOwner" class="mt-4 text-xs text-muted">Only the owner can invite people or change roles.</p>
      </template>
    </template>

    <template #actions>
      <Btn v-if="entry && !isOwner && people" variant="outline" class="text-danger" :disabled="busy" @click="leaving = true">Leave this notebook</Btn>
      <Btn @click="emit('close')">Done</Btn>
    </template>

  </Modal>
  <ConfirmDialog v-if="removing" danger title="Remove this person?" confirm="Remove"
    :message="`${memberName(removing)} will stop receiving updates and can no longer open this notebook here. They keep any copy they already downloaded.`"
    @close="removing = null" @confirm="confirmRemove" />
  <ConfirmDialog v-if="leaving" danger title="Leave this notebook?" confirm="Leave"
    message="You'll lose access. Nothing is deleted for the others; this device keeps what it has until you remove it."
    @close="leaving = false" @confirm="confirmLeave" />
</template>
