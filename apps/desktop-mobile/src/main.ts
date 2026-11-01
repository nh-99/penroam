import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import "./styles/main.css";

// Text blocks are measured with bundled metrics and drawn with the bundled
// font; wait for it so the first canvas paint uses the right glyphs.
const fontReady = document.fonts?.load("16px PenroamSans").catch(() => undefined);

void Promise.resolve(fontReady).finally(() => {
  createApp(App).use(createPinia()).mount("#app");
});
