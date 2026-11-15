// The steps of the guided tour (stores/tour.ts, drawn by Tour.vue).
//
// A step points at the first visible element matching one of its selectors.
// Selectors use the accessible names the controls already have, so they follow
// the UI without extra markup. A step whose target isn't on screen (a phone
// layout keeps some buttons in a menu, a canvas notebook has no page arrows) is
// left out, and a step with no selectors is shown in the middle of the window.
import type { TourName } from "../stores/tour";

/** One step of a tour. */
export interface TourStep {
  /** CSS selectors for the element to point at, tried in order. Empty: no target. */
  target: string[];
  title: string;
  body: string;
}

const mod = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform) ? "Cmd" : "Ctrl";

export const TOURS: Record<TourName, TourStep[]> = {
  library: [
    {
      target: [],
      title: "Welcome to Penroam",
      body: "Penroam is a notebook for writing by hand. Your notes are saved on this device as you write, and nothing needs an account. This short tour shows you around.",
    },
    {
      target: ['[aria-label="New notebook"]'],
      title: "Start a notebook",
      body: "Choose a page size and paper: blank, ruled, grid or dotted, or an infinite canvas that grows as you write.",
    },
    {
      target: ['[aria-label="Import notebooks and documents"]'],
      title: "Bring your notes with you",
      body: "Import PDFs to write on, .penroam notebooks, and notes from Xournal++, Evernote, Joplin, Markdown or text files.",
    },
    {
      target: ['[aria-label="Search notebooks"]'],
      title: "Find anything",
      body: `Search every notebook's titles, typed text, PDFs and handwriting at once (${mod}+K).`,
    },
    {
      target: ['[aria-label="New folder"]', '[aria-label="More actions"]'],
      title: "Folders and trash",
      body: "Group notebooks in folders. A deleted notebook waits in the trash until you delete it for good.",
    },
    {
      target: ['header [aria-label="Settings"]'],
      title: "Make it yours",
      body: "Choose a light, dark or ePaper theme, your pen colours and how touch works. Backups, sync and search live here too, and so does this tour if you want it again.",
    },
  ],
  editor: [
    {
      target: ['[role="radiogroup"][aria-label="Tool"]'],
      title: "Your tools",
      body: "Pen (P), highlighter (M), eraser (E), select (V), lasso (L), text (T) and pan (H). A pen's eraser end erases too.",
    },
    {
      target: ['[role="group"][aria-label="Colour"]'],
      title: "Colours and sizes",
      body: "Pick a colour and a thickness for the tool you're using. The palette button mixes any colour you like.",
    },
    {
      target: ['[aria-label="Ruler"]'],
      title: "Straight lines",
      body: "The ruler (R) lies over the page for straight lines and angles. Drag it to move it, turn the dial in the middle to rotate it, and start a pen stroke on it to draw along its edge.",
    },
    {
      target: ['[aria-label="Text"][role="radio"]', '[aria-label="Text"]'],
      title: "Type next to your ink",
      body: "With the text tool, click anywhere to type. Text understands Markdown, LaTeX formulas like $E = mc^2$, and tables that work as spreadsheets.",
    },
    {
      target: ["aside", '[aria-label="Hide pages"]', '[aria-label="Show pages"]'],
      title: "Pages",
      body: `Add, reorder and delete pages here (${mod}+Shift+N adds one). Scroll down past a page to reach the next.`,
    },
    {
      target: ['[aria-label="Page template"]'],
      title: "Paper and infinite canvas",
      body: "Change a page's paper, or turn the notebook into an infinite canvas with no edges.",
    },
    {
      target: ['[aria-label="Import PDF as pages"]', '[aria-label="Import PDF onto this canvas"]'],
      title: "Write on PDFs",
      body: "Add a PDF's pages to this notebook, then highlight and write on them.",
    },
    {
      target: ['[aria-label="Export and print"]'],
      title: "Export and print",
      body: "Save a notebook as a .penroam file for another device, or as an annotated PDF, Markdown, text or an image. Print from here too.",
    },
    {
      target: ['[aria-label="Version history"]'],
      title: "Go back in time",
      body: "Every page keeps its earlier versions. Look back and restore a page or the whole notebook.",
    },
    {
      target: ['header [role="status"]'],
      title: "No save button",
      body: "Everything is saved as you write, and this shows when it's safe on disk. Even after a crash, your notebook comes back as you left it.",
    },
  ],
};
