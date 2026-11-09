// The editor for a text box: a CodeMirror view holding the block's Markdown source, with live
// formatting (markdownPreview.ts) and the toolbar's commands (markdownFormat.ts).
//
// The engine owns one host element, positions it over the page, and creates one of these for each
// edit. The view sits inside the host because CodeMirror forces its own root to be
// `position: relative`. Its size and font come from the host, so it lines up with the block the
// same way whatever the formatting. Undo inside a box is CodeMirror's; the page's undo history gets
// the batched result when the engine commits.
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { activeFormats, type FormatAction, formatTransaction, taskToggleAt } from "./markdownFormat";
import { HEADING_SCALE, rowsFor } from "./markdown";
import { livePreview, markdownSyntax } from "./markdownPreview";
import { lineHeight } from "./textLayout";

export interface MarkdownEditorOptions {
  text: string;
  fontSize: number;
  /** The paper's row height, for text locked to rows. */
  rowPitch?: number;
  /** The source changed. */
  onChange(text: string): void;
  /** The commands that are on at the selection may have changed. */
  onFormats(formats: FormatAction[]): void;
  /** The rendered height may have changed without the source changing. */
  onResize(): void;
}

const theme = EditorView.theme({
  "&": { background: "transparent", color: "inherit" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { overflow: "visible", fontFamily: "inherit", lineHeight: "inherit" },
  ".cm-content": { padding: "0", caretColor: "currentColor" },
  ".cm-line": { padding: "0" },
});

/** A live Markdown editor mounted in `host`, with the caret at the end of the text. */
export class MarkdownEditor {
  readonly view: EditorView;

  constructor(host: HTMLElement, opts: MarkdownEditorOptions) {
    HEADING_SCALE.forEach((scale, i) => {
      const size = opts.fontSize * scale;
      host.style.setProperty(`--md-h${i + 1}-lh`, `${opts.rowPitch === undefined ? lineHeight(size) : rowsFor(size, opts.rowPitch) * opts.rowPitch}px`);
    });
    const run = (action: FormatAction) => () => this.format(action);
    this.view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: opts.text,
        selection: { anchor: opts.text.length },
        extensions: [
          history(),
          markdownSyntax(),
          livePreview(),
          theme,
          EditorView.lineWrapping,
          EditorView.contentAttributes.of({ "aria-label": "Text block", spellcheck: "true" }),
          keymap.of([{ key: "Mod-b", run: run("bold") }, { key: "Mod-i", run: run("italic") }, { key: "Mod-k", run: run("link") }, ...historyKeymap, ...defaultKeymap]),
          // the page moves by its own transform, and scrolling its containers would shift it
          EditorView.scrollHandler.of(() => true),
          EditorView.domEventHandlers({
            mousedown: (e, view) => {
              const box = (e.target as HTMLElement).closest?.(".cm-md-task");
              const toggle = box && taskToggleAt(view.state, view.posAtDOM(box));
              if (!toggle) return false;
              e.preventDefault();
              view.dispatch(toggle);
              return true;
            },
          }),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) opts.onChange(u.state.doc.toString());
            else if (u.geometryChanged || u.heightChanged) opts.onResize();
            if (u.docChanged || u.selectionSet) opts.onFormats(activeFormats(u.state));
          }),
        ],
      }),
    });
  }

  /** The rendered height in page units; 0 before the first layout. */
  get height(): number {
    return this.view.contentDOM.offsetHeight;
  }

  /** The commands that are on at the selection. */
  get formats(): FormatAction[] {
    return activeFormats(this.view.state);
  }

  focus() {
    this.view.focus();
  }

  /** Runs a toolbar command. Always reports handled, as a keyboard shortcut must not fall through. */
  format(action: FormatAction): boolean {
    const tr = formatTransaction(this.view.state, action);
    if (tr) this.view.dispatch(tr);
    this.view.focus();
    return true;
  }

  destroy() {
    this.view.destroy();
  }
}
