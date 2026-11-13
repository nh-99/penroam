<script setup lang="ts">
// A notebook somebody else owns, in the library's "Shared with me" section. It shows the role and
// where this device stands (Joining…, Synced, Access removed, …). Access that was removed can't be
// opened for editing; the card offers keeping a private copy or dropping this device's copy.
import { LoaderCircle, EllipsisVertical, Users, Download } from "@lucide/vue";
import type { NotebookSummary } from "../generated/NotebookSummary";
import { roleLabel, sharedStatusLabel, type SharedNotebookView } from "../platform/sharing";
import Btn from "./ui/Btn.vue";
import { cleanName } from "../platform/display";
import Menu from "./ui/Menu.vue";
import MenuItem from "./ui/MenuItem.vue";

const props = defineProps<{ shared: SharedNotebookView; nb?: NotebookSummary; cover?: string; busy?: boolean }>();
const emit = defineEmits<{ open: []; people: []; export: []; keepCopy: []; removeLocal: []; resolve: [] }>();
const title = () => props.nb?.title ?? props.shared.title;
</script>

<template>
  <article class="flex flex-col">
    <button
      class="relative block aspect-[17/22] w-full overflow-hidden rounded-xl border border-line bg-white text-left shadow-sm transition hover:-translate-y-0.5 hover:shadow-md focus-visible:-translate-y-0.5 disabled:translate-y-0 disabled:opacity-70 disabled:shadow-none epaper:border-2 epaper:border-black"
      :class="shared.state === 'joining' ? 'border-dashed' : ''"
      :aria-label="shared.state === 'joining' ? `Joining ${title()}` : `Open ${title()}`"
      :disabled="shared.state === 'joining' || !nb"
      @click="emit('open')"
    >
      <div v-if="nb" class="absolute inset-y-0 left-0 w-3" :style="{ background: nb.cover }" aria-hidden="true" />
      <img v-if="cover" :src="cover" alt="" class="h-full w-full object-cover object-top pl-3" draggable="false" />
      <div v-else class="flex h-full w-full items-center justify-center pl-3 text-muted">
        <LoaderCircle v-if="shared.state === 'joining'" :size="32" class="animate-spin" aria-hidden="true" />
        <Users v-else :size="32" aria-hidden="true" />
      </div>
    </button>
    <div class="mt-2 flex items-start gap-1">
      <div class="min-w-0 flex-1">
        <h3 class="truncate text-sm font-medium" :title="title()">{{ title() }}</h3>
        <p class="flex flex-wrap items-center gap-x-1.5 text-xs text-muted">
          <span class="rounded-full border border-line px-1.5 epaper:border-black epaper:text-black" data-testid="role-chip">{{ roleLabel(shared.role) }}</span>
          <span data-testid="state-label">{{ sharedStatusLabel(shared) }}</span>
        </p>
        <p v-if="shared.ownerDisplay" class="truncate text-xs text-muted">From <bdi>{{ cleanName(shared.ownerDisplay) }}</bdi></p>
        <button v-if="shared.syncState === 'needsResolution' && shared.state === 'active'" type="button" class="mt-1 text-xs underline" @click="emit('resolve')">Resolve differences</button>
      </div>
      <Menu v-if="shared.state === 'active'" :label="`${title()} actions`">
        <template #trigger="{ toggle, open }">
          <button class="touch-target -mr-2 -mt-1 inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-expanded="open" aria-haspopup="menu" :aria-label="`More actions for ${title()}`" @click="toggle">
            <EllipsisVertical :size="18" />
          </button>
        </template>
        <MenuItem @click="emit('people')"><Users :size="16" />People and leaving…</MenuItem>
        <MenuItem @click="emit('export')"><Download :size="16" />Export .penroam…</MenuItem>
      </Menu>
    </div>
    <div v-if="shared.state === 'accessRemoved'" class="mt-2 flex flex-col gap-1.5">
      <Btn variant="outline" :disabled="busy" @click="emit('keepCopy')">Keep a private copy</Btn>
      <Btn variant="ghost" class="text-danger" :disabled="busy" @click="emit('removeLocal')">Remove from this device</Btn>
    </div>
  </article>
</template>
