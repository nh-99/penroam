<script setup lang="ts">
// Asked when a notebook with several pages becomes an infinite canvas: keep its pages as
// separate canvases, or gather them all onto the first one.
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
defineProps<{ pages: number }>();
const emit = defineEmits<{ close: []; choose: [collapse: boolean] }>();
</script>

<template>
  <Modal title="Turn on infinite canvas" @close="emit('close')">
    <p class="text-sm leading-relaxed text-muted">
      Each of the {{ pages }} pages can stay a canvas of its own, or they can all go onto one canvas, one below the other.
      Combined pages keep their writing and PDF pages; the emptied pages go to this notebook's trash, where you can restore them.
    </p>
    <template #actions>
      <Btn @click="emit('close')">Cancel</Btn>
      <Btn @click="emit('choose', false)">Keep {{ pages }} pages</Btn>
      <Btn variant="primary" autofocus @click="emit('choose', true)">Combine into one canvas</Btn>
    </template>
  </Modal>
</template>
