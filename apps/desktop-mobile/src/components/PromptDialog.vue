<script setup lang="ts">
import { ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";

const props = defineProps<{ title: string; label: string; value: string; confirm: string }>();
const emit = defineEmits<{ close: []; submit: [v: string] }>();
const v = ref(props.value);
</script>

<template>
  <Modal :title="title" @close="emit('close')">
    <form @submit.prevent="v.trim() && emit('submit', v.trim())">
      <label class="block">
        <span class="mb-1.5 block text-sm font-medium">{{ label }}</span>
        <input v-model="v" autofocus maxlength="300" class="h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" />
      </label>
      <div class="mt-6 flex justify-end gap-2">
        <Btn @click="emit('close')">Cancel</Btn>
        <Btn variant="primary" type="submit" :disabled="!v.trim()">{{ confirm }}</Btn>
      </div>
    </form>
  </Modal>
</template>
