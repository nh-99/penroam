<script setup lang="ts">
// A pending invitation. The notebook's title is sealed until the invitation is accepted, so the
// card only says who invited you and what role you would have.
import { Mail } from "@lucide/vue";
import { roleLabel, type IncomingInvitationView } from "../platform/sharing";
import Btn from "./ui/Btn.vue";
import { cleanName } from "../platform/display";

defineProps<{ invitation: IncomingInvitationView; busy?: boolean; problem?: "reauth" | "oldCopy" }>();
const emit = defineEmits<{ accept: []; decline: []; signIn: []; keepCopy: []; removeLocal: [] }>();
</script>

<template>
  <li class="flex flex-wrap items-center gap-3 rounded-xl border border-line bg-surface px-4 py-3 epaper:border-2 epaper:border-black">
    <Mail :size="20" class="shrink-0 text-muted" aria-hidden="true" />
    <div class="min-w-0 flex-1 basis-48">
      <p class="truncate text-sm font-medium"><bdi>{{ cleanName(invitation.invitedBy) }}</bdi> invited you to a notebook</p>
      <p class="text-xs text-muted">You would have: {{ roleLabel(invitation.role) }}. The title shows after you accept.</p>
    </div>
    <div v-if="problem === 'reauth'" class="flex w-full flex-wrap items-center gap-2" role="alert">
      <p class="min-w-0 flex-1 basis-48 text-sm">Sign in again to confirm your email address, then accept.</p>
      <Btn variant="outline" @click="emit('signIn')">Sign in again</Btn>
    </div>
    <div v-if="problem === 'oldCopy'" class="flex w-full flex-wrap items-center gap-2" role="alert">
      <p class="min-w-0 flex-1 basis-48 text-sm">This device still has your earlier copy of this notebook. Keep it as a private copy or remove it, then accept again.</p>
      <Btn variant="outline" :disabled="busy" @click="emit('keepCopy')">Keep a private copy</Btn>
      <Btn variant="ghost" class="text-danger" :disabled="busy" @click="emit('removeLocal')">Remove from this device</Btn>
    </div>
    <Btn variant="outline" :disabled="busy" :aria-label="`Decline invitation from ${cleanName(invitation.invitedBy)}`" @click="emit('decline')">Decline</Btn>
    <Btn variant="primary" :disabled="busy" :aria-label="`Accept invitation from ${cleanName(invitation.invitedBy)}`" @click="emit('accept')">Accept</Btn>
  </li>
</template>
