// Live Markdown preview for the text box editor: CodeMirror decorations over the source.
//
// The document is always the Markdown source, and only its appearance changes. Formatting applies
// on every line as soon as it parses; the markers themselves (`**`, `# `, `- [ ] `, ...) are hidden
// except on lines the selection touches, so the source stays reachable where you are working.
//
// This is a preview, not the layout of record. The finished block is drawn by `markdown.ts`, which
// has its own parser and is checked against penroam-core. Lezer is only used here, and where the
// two parsers differ the preview is the one that is approximate. Formulas are the exception to
// parser independence: they share `scanMath`, so `$a*b*c$` can't turn italic here. Tables and
// formulas stay as source while editing, tables in the monospace face so their columns line up.
// See docs/architecture/overview.md.
import { commonmarkLanguage, markdown } from "@codemirror/lang-markdown";
import { ensureSyntaxTree, syntaxTree } from "@codemirror/language";
import { type EditorState, type Extension, type Range, StateField } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, WidgetType } from "@codemirror/view";
import { type MarkdownConfig, Strikethrough, Table, TaskList } from "@lezer/markdown";
import { HEADING_SCALE, scanMath } from "./markdown";
import { MONO_FONT_FAMILY, MONO_SCALE } from "./textLayout";

const DOLLAR = 36;

/** `$...$` and `$$...$$` as one inline node, by the same rules the finished layout uses. */
const mathSyntax: MarkdownConfig = {
  defineNodes: ["Math", "MathMark"],
  parseInline: [
    {
      name: "Math",
      parse(cx, next, pos) {
        if (next !== DOLLAR) return -1;
        const chars = Array.from(cx.slice(pos, cx.end));
        const found = scanMath(chars, 0);
        if (!found || !chars.slice(found[0], found[1]).join("").trim()) return -1;
        const end = pos + chars.slice(0, found[2]).join("").length;
        const open = found[3] ? 2 : 1;
        return cx.addElement(cx.elt("Math", pos, end, [cx.elt("MathMark", pos, pos + open), cx.elt("MathMark", end - open, end)]));
      },
    },
  ],
};

/** The Markdown dialect of the editor: what Penroam renders, with no GFM extras it doesn't. */
export function markdownSyntax(): Extension {
  return markdown({ base: commonmarkLanguage, extensions: [Table, TaskList, Strikethrough, mathSyntax] });
}

class TextWidget extends WidgetType {
  constructor(private text: string) {
    super();
  }
  eq(o: TextWidget) {
    return o.text === this.text;
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-md-bullet";
    el.textContent = this.text;
    return el;
  }
}

class TaskWidget extends WidgetType {
  constructor(private done: boolean) {
    super();
  }
  eq(o: TaskWidget) {
    return o.done === this.done;
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-md-task";
    el.setAttribute("role", "checkbox");
    el.setAttribute("aria-checked", String(this.done));
    el.textContent = this.done ? "✓" : "";
    return el;
  }
  ignoreEvent() {
    return false;
  }
}

class RuleWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-md-rule";
    return el;
  }
}

const hidden = Decoration.replace({});
const rule = Decoration.replace({ widget: new RuleWidget() });

function activeLines(state: EditorState): Set<number> {
  const lines = new Set<number>();
  for (const r of state.selection.ranges) {
    const last = state.doc.lineAt(r.to).number;
    for (let n = state.doc.lineAt(r.from).number; n <= last; n++) lines.add(n);
  }
  return lines;
}

/** All the preview's decorations for a state. Works without a view. */
export function previewDecorations(state: EditorState): DecorationSet {
  const doc = state.doc;
  const active = activeLines(state);
  // Boxes are short, so parsing to the end now beats redrawing when a background parse lands.
  const tree = ensureSyntaxTree(state, doc.length, 250) ?? syntaxTree(state);
  const out: Range<Decoration>[] = [];

  const revealed = (pos: number) => active.has(doc.lineAt(pos).number);
  const mark = (cls: string, from: number, to: number) => {
    if (from < to) out.push(Decoration.mark({ class: cls }).range(from, to));
  };
  const hide = (from: number, to: number) => {
    if (from < to) out.push(hidden.range(from, to));
  };
  const eachLine = (cls: string, from: number, to: number) => {
    const last = doc.lineAt(to).number;
    for (let n = doc.lineAt(from).number; n <= last; n++) out.push(Decoration.line({ class: cls }).range(doc.line(n).from));
  };
  const withSpace = (to: number) => (doc.sliceString(to, to + 1) === " " ? to + 1 : to);
  /** A marker: faint where the source shows, and gone with its trailing space elsewhere. */
  const marker = (from: number, to: number, spaceAfter = false) => {
    if (revealed(from)) return mark("cm-md-syntax", from, to);
    hide(from, spaceAfter ? withSpace(to) : to);
  };

  tree.iterate({
    enter: (n) => {
      const heading = /^(ATX|Setext)Heading(\d)$/.exec(n.name);
      if (heading) {
        const cls = `cm-md-h${heading[2]}`;
        // a setext heading's underline is not part of the heading's text
        const underline = heading[1] === "Setext" ? n.node.getChild("HeaderMark") : null;
        eachLine(cls, n.from, underline ? underline.from - 1 : n.to);
        return;
      }
      switch (n.name) {
        case "HeaderMark":
          if (n.node.parent?.name.startsWith("Setext")) mark("cm-md-syntax", n.from, n.to);
          else marker(n.from, n.to, true);
          return;
        case "StrongEmphasis":
          return mark("cm-md-strong", n.from, n.to);
        case "Emphasis":
          return mark("cm-md-em", n.from, n.to);
        case "Strikethrough":
          return mark("cm-md-strike", n.from, n.to);
        case "InlineCode":
          return mark("cm-md-code", n.from, n.to);
        case "EmphasisMark":
        case "StrikethroughMark":
        case "CodeMark":
          return marker(n.from, n.to);
        case "Escape":
          if (!revealed(n.from)) hide(n.from, n.from + 1);
          return;
        case "Math":
          mark("cm-md-math", n.from, n.to);
          return false;
        case "Blockquote":
          return eachLine("cm-md-quote", n.from, n.to);
        case "QuoteMark":
          return marker(n.from, n.to, true);
        case "ListItem":
          out.push(Decoration.line({ class: "cm-md-li" }).range(doc.lineAt(n.from).from));
          return;
        case "ListMark": {
          const item = n.node.parent!;
          const task = item.getChild("Task");
          if (revealed(n.from)) return mark("cm-md-syntax", n.from, n.to);
          if (task) return hide(n.from, task.from);
          if (item.parent?.name !== "BulletList") return;
          let depth = 0;
          for (let p = item.parent; p; p = p.parent!) if (p.name === "BulletList" || p.name === "OrderedList") depth++;
          // the widget is the whole gutter, so the space after the marker goes with it
          out.push(Decoration.replace({ widget: new TextWidget(depth % 2 === 1 ? "•" : "–") }).range(n.from, withSpace(n.to)));
          return;
        }
        case "TaskMarker":
          if (!revealed(n.from)) out.push(Decoration.replace({ widget: new TaskWidget(doc.sliceString(n.from + 1, n.to - 1) !== " ") }).range(n.from, withSpace(n.to)));
          return;
        case "Link": {
          // without a destination this is a bare `[text]`, which the finished layout leaves alone
          if (!n.node.getChild("URL")) return;
          const [open, close] = n.node.getChildren("LinkMark");
          mark("cm-md-link", open.to, close.from);
          marker(open.from, open.to);
          marker(close.from, n.to);
          return;
        }
        case "Autolink": {
          mark("cm-md-link", n.from, n.to);
          const marks = n.node.getChildren("LinkMark");
          for (const m of marks) marker(m.from, m.to);
          return;
        }
        case "HorizontalRule":
          if (revealed(n.from)) mark("cm-md-syntax", n.from, n.to);
          else out.push(rule.range(n.from, n.to));
          return;
        case "FencedCode":
          eachLine("cm-md-codeblock", n.from, n.to);
          for (let c = n.node.firstChild; c; c = c.nextSibling) if (c.name === "CodeMark" || c.name === "CodeInfo") mark("cm-md-syntax", c.from, c.to);
          return false;
        case "CodeBlock":
          eachLine("cm-md-codeblock", n.from, n.to);
          return false;
        case "Table": {
          eachLine("cm-md-table", n.from, n.to);
          const header = n.node.getChild("TableHeader");
          if (header) mark("cm-md-strong", header.from, header.to);
          n.node.cursor().iterate((c) => {
            if (c.name === "TableDelimiter") mark("cm-md-syntax", c.from, c.to);
          });
          return false;
        }
      }
    },
  });
  return Decoration.set(out, true);
}

function lineKey(state: EditorState): string {
  return [...activeLines(state)].join(",");
}

const previewField = StateField.define<{ key: string; set: DecorationSet }>({
  create: (state) => ({ key: lineKey(state), set: previewDecorations(state) }),
  update(value, tr) {
    const key = lineKey(tr.state);
    // moving within a line changes nothing, and that is most selection updates
    return !tr.docChanged && key === value.key ? value : { key, set: previewDecorations(tr.state) };
  },
  provide: (f) => EditorView.decorations.from(f, (v) => v.set),
});

const mono = { fontFamily: MONO_FONT_FAMILY, fontSize: `${MONO_SCALE}em` };
const codeTint = "rgba(127, 127, 127, 0.16)";

const previewTheme = EditorView.theme({
  ...Object.fromEntries(HEADING_SCALE.map((scale, i) => [`.cm-md-h${i + 1}`, { fontSize: `${scale}em`, lineHeight: `var(--md-h${i + 1}-lh)`, fontWeight: "bold" }])),
  ".cm-md-syntax": { opacity: "0.5" },
  ".cm-md-strong": { fontWeight: "bold" },
  ".cm-md-em": { fontStyle: "italic" },
  ".cm-md-strike": { textDecoration: "line-through" },
  ".cm-md-link": { textDecoration: "underline" },
  ".cm-md-code": { ...mono, background: codeTint, borderRadius: "0.2em" },
  ".cm-md-math": { ...mono, background: codeTint, borderRadius: "0.2em" },
  ".cm-md-codeblock": { ...mono, background: codeTint },
  ".cm-md-table": mono,
  ".cm-md-quote": { borderLeft: "0.12em solid currentColor", paddingLeft: "0.65em" },
  // wrapped lines of an item hang under its text, as they do in the finished layout
  ".cm-md-li": { paddingLeft: "1.5em", textIndent: "-1.5em" },
  ".cm-md-bullet": { display: "inline-block", boxSizing: "border-box", width: "1.5em", paddingRight: "0.4em", textAlign: "right", textIndent: "0" },
  ".cm-md-task": {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    boxSizing: "border-box",
    width: "0.68em",
    height: "0.68em",
    marginRight: "0.82em",
    border: "0.06em solid currentColor",
    fontSize: "1em",
    lineHeight: "1",
    textIndent: "0",
    cursor: "pointer",
    verticalAlign: "baseline",
  },
  ".cm-md-rule": { display: "inline-block", width: "100%", borderTop: "0.06em solid currentColor", verticalAlign: "middle" },
});

/** Formatting as you type, in a CodeMirror editor that also has [`markdownSyntax`]. */
export function livePreview(): Extension {
  return [previewField, previewTheme];
}
