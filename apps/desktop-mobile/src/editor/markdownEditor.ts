// The editor for a text box: a CodeMirror view holding the block's Markdown source, with live
// formatting (markdownPreview.ts) and the toolbar's commands (markdownFormat.ts).
//
// The engine owns one host element, positions it over the page, and creates one of these for each
// edit. The view sits inside the host because CodeMirror forces its own root to be
// `position: relative`. Its size and font come from the host, so it lines up with the block the
// same way whatever the formatting. Undo inside a box is CodeMirror's; the page's undo history gets
// the batched result when the engine commits.
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { EditorState, Transaction } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { activeFormats, type FormatAction, formatTransaction, taskToggleAt } from "./markdownFormat";
import { HEADING_SCALE, rowsFor } from "./markdown";
import { livePreview, markdownSyntax } from "./markdownPreview";
import { focusedGrid } from "./sheetGrid";
import { lineHeight } from "./textLayout";
import { diffSplices } from "./textHistory";

export interface MarkdownEditorOptions {
  text: string;
  fontSize: number;
  /** The paper's row height, for text locked to rows. */
  rowPitch?: number;
  /** The source changed. `remote`: a collaborator's change was merged in, not typing. */
  onChange(text: string, remote?: boolean): void;
  /** The caret or selection moved, or the text under it changed. */
  onSelection?(view: EditorView): void;
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
            if (u.docChanged) opts.onChange(u.state.doc.toString(), u.transactions.some((t) => t.annotation(Transaction.remote) === true));
            else if (u.geometryChanged || u.heightChanged) opts.onResize();
            if (u.docChanged || u.selectionSet) {
              opts.onFormats(activeFormats(u.state));
              opts.onSelection?.(u.view);
            }
          }),
        ],
      }),
    });
  }

  /** The rendered height in page units; 0 before the first layout. */
  get height(): number {
    return this.view.contentDOM.offsetHeight;
  }

  /** The commands that are on at the selection, or for the selected cells of a table being worked in. */
  get formats(): FormatAction[] {
    return this.grid()?.formats ?? activeFormats(this.view.state);
  }

  /** The table in this box that has focus, if any. */
  private grid() {
    const g = focusedGrid();
    return g && this.view.dom.contains(g.root) ? g : null;
  }

  focus() {
    this.view.focus();
  }

  /** The current source. */
  get text(): string {
    return this.view.state.doc.toString();
  }

  /**
   * Merges a collaborator's version of the text in, changing only what differs. The caret and
   * selection map through the change, composition isn't touched (the caller waits for it to end),
   * and the change is kept out of this box's undo history, which CodeMirror rebases over it, so
   * undo removes only what was typed here.
   */
  applyRemote(target: string) {
    const doc = this.view.state.doc.toString();
    const splices = diffSplices(doc, target);
    if (!splices.length) return;
    this.view.dispatch({
      changes: splices.map((s) => ({ from: s.from, to: s.to, insert: s.insert })),
      annotations: [Transaction.remote.of(true), Transaction.addToHistory.of(false)],
    });
  }

  /** Runs a toolbar command. Always reports handled, as a keyboard shortcut must not fall through. */
  format(action: FormatAction): boolean {
    // while a table has focus, the toolbar formats its cells
    const grid = this.grid();
    if (grid) return grid.formatCommand(action);
    const tr = formatTransaction(this.view.state, action);
    if (tr) this.view.dispatch(tr);
    this.view.focus();
    return true;
  }

  destroy() {
    this.view.destroy();
  }
}
