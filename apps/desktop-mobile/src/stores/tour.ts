// The guided tour: a few steps that point at parts of the library or the editor
// and say what they do (steps in components/tourSteps.ts, drawn by Tour.vue).
//
// It starts by itself only for someone new: the first time the app opens on an
// empty library. Finishing or closing that library tour leaves the editor tour
// pending, and it runs the first time a notebook is opened. Anyone else gets
// the tour only by asking for it (Settings → General → Take the tour), so an
// existing library, a test run or a screenshot session is never interrupted.
import { defineStore } from "pinia";
import { ref } from "vue";
import { useSettings } from "./settings";

/** The view a tour belongs to. */
export type TourName = "library" | "editor";

export const useTour = defineStore("tour", () => {
  const settings = useSettings();
  /** The tour on screen, or null. */
  const active = ref<TourName | null>(null);

  /** Shows `name` from its first step, replacing any tour on screen. */
  function start(name: TourName) {
    active.value = name;
  }

  /**
   * Closes the tour on screen. `finished` is true when the last step was reached;
   * closing early also cancels a pending editor tour, since someone who skips one
   * tour doesn't want the next.
   */
  function stop(finished: boolean) {
    if (!finished) settings.s.editorTourPending = false;
    active.value = null;
  }

  /**
   * Called once the library has loaded at start-up. The first start on an empty
   * library begins the library tour and leaves the editor tour pending; any
   * other first start just records that the offer was made.
   */
  function offerAtStartup(libraryEmpty: boolean) {
    if (settings.s.tourOffered) return;
    settings.s.tourOffered = true;
    if (!libraryEmpty) return;
    settings.s.editorTourPending = true;
    start("library");
  }

  /** Called when a notebook opens: runs the editor tour if it is pending. */
  function offerInEditor() {
    if (!settings.s.editorTourPending || active.value) return;
    settings.s.editorTourPending = false;
    start("editor");
  }

  /**
   * Runs the first-start onboarding again, whatever the library holds: the
   * library tour now and the editor tour when a notebook next opens. Developer
   * mode uses this to check the flow without wiping the library. Call it from
   * the library view, since the library tour points at its controls.
   */
  function replayOnboarding() {
    settings.s.tourOffered = true;
    settings.s.editorTourPending = true;
    start("library");
  }

  return { active, start, stop, offerAtStartup, offerInEditor, replayOnboarding };
});
