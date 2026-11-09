<script setup lang="ts">
// Chooses one colour with hue, saturation and brightness sliders or a typed hex value. Built in
// rather than <input type="color"> because the system picker differs on each platform and isn't
// dependable in every WebView. As a Modal, it also hides native ink layers while open.
import { computed, reactive, ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { hexToHsv, hsvToHex, normalizeHex } from "../editor/color";

const props = defineProps<{ title: string; color: string; confirm: string; removable?: boolean }>();
const emit = defineEmits<{ close: []; pick: [color: string]; remove: [] }>();

// The sliders are the source of truth, so dragging saturation to zero and back keeps the hue.
const hsv = reactive(hexToHsv(props.color));
const hexText = ref(props.color);
const hex = ref(props.color);
const valid = computed(() => normalizeHex(hexText.value) !== null);

function sliderMoved() {
  hex.value = hsvToHex(hsv);
  hexText.value = hex.value;
}
function hueMoved() {
  // Greys and near-blacks show no hue at all, and the pen starts on #111111: without this, the
  // first drag of the rainbow slider would appear to do nothing.
  if (hsv.s === 0) hsv.s = 100;
  if (hsv.v < 15) hsv.v = 100;
  sliderMoved();
}
function hexTyped() {
  const typed = normalizeHex(hexText.value);
  if (!typed) return;
  hex.value = typed;
  Object.assign(hsv, hexToHsv(typed));
}
function submit() {
  if (valid.value) emit("pick", hex.value);
}

const hueTrack = "linear-gradient(to right, #f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00)";
const saturationTrack = computed(() => `linear-gradient(to right, ${hsvToHex({ ...hsv, s: 0 })}, ${hsvToHex({ ...hsv, s: 100 })})`);
const brightnessTrack = computed(() => `linear-gradient(to right, #000, ${hsvToHex({ ...hsv, v: 100 })})`);
</script>

<template>
  <Modal :title="title" @close="emit('close')">
    <form class="space-y-3" @submit.prevent="submit">
      <!-- Hue first and focused, not the hex field: focusing a text field raises a phone's keyboard. -->
      <label class="block">
        <span class="mb-1 block text-sm">Hue</span>
        <input v-model.number="hsv.h" autofocus type="range" min="0" max="360" class="color-slider" :style="{ background: hueTrack }" @input="hueMoved" />
      </label>
      <label class="block">
        <span class="mb-1 block text-sm">Saturation</span>
        <input v-model.number="hsv.s" type="range" min="0" max="100" class="color-slider" :style="{ background: saturationTrack }" @input="sliderMoved" />
      </label>
      <label class="block">
        <span class="mb-1 block text-sm">Brightness</span>
        <input v-model.number="hsv.v" type="range" min="0" max="100" class="color-slider" :style="{ background: brightnessTrack }" @input="sliderMoved" />
      </label>
      <div class="flex items-center gap-3 pt-1">
        <span class="h-11 w-11 shrink-0 rounded-full border border-black/15" data-testid="color-preview" :style="{ background: hex }" />
        <label class="min-w-0 flex-1">
          <span class="sr-only">Hex colour</span>
          <input v-model="hexText" maxlength="7" spellcheck="false" autocapitalize="off" autocomplete="off" :aria-invalid="!valid" class="h-11 w-full rounded-xl border border-line bg-surface px-3 text-base outline-none focus:border-brand epaper:border-2" @input="hexTyped" />
        </label>
      </div>
      <p v-if="!valid" class="text-xs text-danger" role="alert">Enter a colour like #1d4ed8.</p>
      <div class="flex justify-end gap-2 pt-3">
        <Btn v-if="removable" variant="danger" class="mr-auto" @click="emit('remove')">Remove</Btn>
        <Btn @click="emit('close')">Cancel</Btn>
        <Btn variant="primary" type="submit" :disabled="!valid">{{ confirm }}</Btn>
      </div>
    </form>
  </Modal>
</template>
