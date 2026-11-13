<script setup lang="ts">
// The header button for notifications, with the unread count as text as well as a dot.
import { Bell } from "@lucide/vue";
import IconBtn from "./ui/IconBtn.vue";
import { useComments } from "../stores/comments";
import { useSharing } from "../stores/sharing";

const store = useComments();
const sharing = useSharing();
// notifications come from shared notebooks: nothing to show where the server offers no sharing
const shown = () => sharing.available || sharing.notebooks.length > 0 || store.notifications.length > 0;
</script>

<template>
  <span v-if="shown()" class="relative inline-flex">
    <IconBtn :label="store.unreadNotifications ? `Notifications, ${store.unreadNotifications} unread` : 'Notifications'" @click="store.centerOpen = true"><Bell :size="20" /></IconBtn>
    <span v-if="store.unreadNotifications" class="pointer-events-none absolute right-0.5 top-0.5 min-w-4 rounded-full bg-danger px-1 text-center text-[10px] font-bold leading-4 text-white epaper:border epaper:border-black epaper:bg-black" data-testid="notification-badge" aria-hidden="true">
      {{ store.unreadNotifications > 99 ? "99+" : store.unreadNotifications }}
    </span>
  </span>
</template>
