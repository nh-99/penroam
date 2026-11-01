<script setup lang="ts">
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
defineProps<{ title: string; message: string; confirm: string; danger?: boolean }>();
const emit = defineEmits<{ close: []; confirm: [] }>();
</script>

<template>
  <Modal :title="title" @close="emit('close')">
    <p class="text-sm leading-relaxed text-muted">{{ message }}</p>
    <template #actions>
      <Btn autofocus @click="emit('close')">Cancel</Btn>
      <Btn :variant="danger ? 'danger' : 'primary'" @click="emit('confirm')">{{ confirm }}</Btn>
    </template>
  </Modal>
</template>
