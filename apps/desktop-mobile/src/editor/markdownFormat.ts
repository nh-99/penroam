// The text box formatting commands: what the toolbar's buttons do to the Markdown source.
//
// Everything here is a function of a CodeMirror state and returns a transaction, so it runs and is
// tested without a view. Inline styles and links read the syntax tree, so a button can tell that
// the caret is already inside bold text and turn it off. Line formats work on the text of each
// selected line, so they also apply to a line that doesn't parse as anything yet.
import { ensureSyntaxTree, syntaxTree } from "@codemirror/language";
import { type ChangeSpec, EditorSelection, type EditorState, type TransactionSpec } from "@codemirror/state";
import type { SyntaxNode } from "@lezer/common";

/** A toolbar formatting command. */
export type FormatAction = "bold" | "italic" | "strike" | "code" | "math" | "h1" | "h2" | "h3" | "bullet" | "ordered" | "task" | "quote" | "codeBlock" | "link" | "rule" | "table";

type InlineAction = "bold" | "italic" | "strike" | "code" | "math";
type LineKind = "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "bullet" | "ordered" | "task";

const INLINE: Record<InlineAction, { node: string; mark: string }> = {
  bold: { node: "StrongEmphasis", mark: "**" },
  italic: { node: "Emphasis", mark: "*" },
  strike: { node: "Strikethrough", mark: "~~" },
  code: { node: "InlineCode", mark: "`" },
  math: { node: "Math", mark: "$" },
};

const LINK_PLACEHOLDER = "https://";
const TABLE = "| Column 1 | Column 2 |\n| --- | --- |\n|  |  |";

function treeOf(state: EditorState) {
  return ensureSyntaxTree(state, state.doc.length, 250) ?? syntaxTree(state);
}

function enclosing(state: EditorState, pos: number, side: -1 | 1, name: string): SyntaxNode | null {
  for (let n: SyntaxNode | null = treeOf(state).resolveInner(pos, side); n; n = n.parent) if (n.name === name) return n;
  return null;
}

/** The node of type `name` the selection is inside, if all of it is inside the same one. */
function selected(state: EditorState, name: string): SyntaxNode | null {
  const { from, to, empty } = state.selection.main;
  if (empty) return enclosing(state, from, 1, name) ?? enclosing(state, from, -1, name);
  const a = enclosing(state, from, 1, name);
  const b = enclosing(state, to, -1, name);
  return a && b && a.from === b.from ? a : null;
}

function selectedLink(state: EditorState): SyntaxNode | null {
  const link = selected(state, "Link");
  return link?.getChild("URL") ? link : null;
}

/** Splits a line into what stays (indent and quotes) and its list or heading marker. */
function parseLine(text: string): { kind: LineKind | null; number: number; markerFrom: number; markerTo: number } {
  const markerFrom = /^[ \t]*(?:>[ \t]?)*[ \t]*/.exec(text)![0].length;
  const m = /^(?:(#{1,6})(?:[ \t]+|$)|([-+*])(?:[ \t]+|$)(\[[ xX]\](?:[ \t]+|$))?|(\d{1,9})[.)](?:[ \t]+|$))/.exec(text.slice(markerFrom));
  if (!m) return { kind: null, number: 0, markerFrom, markerTo: markerFrom };
  const kind = m[1] ? (`h${m[1].length}` as LineKind) : m[3] ? "task" : m[2] ? "bullet" : "ordered";
  return { kind, number: Number(m[4] ?? 0), markerFrom, markerTo: markerFrom + m[0].length };
}

function selectedLines(state: EditorState) {
  const { from, to } = state.selection.main;
  const lines = [];
  for (let n = state.doc.lineAt(from).number, last = state.doc.lineAt(to).number; n <= last; n++) lines.push(state.doc.line(n));
  return lines;
}

/** The lines a line format applies to: blank lines only count when there is nothing else. */
function targetLines(state: EditorState) {
  const lines = selectedLines(state);
  const filled = lines.filter((l) => l.text.trim());
  return filled.length ? filled : lines;
}

function toggleInline(state: EditorState, action: InlineAction): TransactionSpec | null {
  const { node, mark } = INLINE[action];
  const inside = selected(state, node);
  if (inside) {
    const changes = state.changes([
      { from: inside.firstChild!.from, to: inside.firstChild!.to },
      { from: inside.lastChild!.from, to: inside.lastChild!.to },
    ]);
    return { changes, selection: state.selection.map(changes) };
  }
  const sel = state.selection.main;
  if (sel.empty) {
    const word = state.wordAt(sel.head);
    if (!word) return { changes: { from: sel.head, insert: mark + mark }, selection: { anchor: sel.head + mark.length } };
    return { changes: [{ from: word.from, insert: mark }, { from: word.to, insert: mark }], selection: { anchor: sel.head + mark.length } };
  }
  // Wrapped line by line: one pair of markers can't span list items or other blocks, and a
  // marker next to a space doesn't count as one in CommonMark.
  const specs: ChangeSpec[] = [];
  for (const line of selectedLines(state)) {
    let start = Math.max(sel.from, line.from + parseLine(line.text).markerTo);
    let end = Math.min(sel.to, line.to);
    while (start < end && /\s/.test(state.doc.sliceString(start, start + 1))) start++;
    while (end > start && /\s/.test(state.doc.sliceString(end - 1, end))) end--;
    if (start < end) specs.push({ from: start, insert: mark }, { from: end, insert: mark });
  }
  if (!specs.length) return null;
  const changes = state.changes(specs);
  return { changes, selection: EditorSelection.single(changes.mapPos(sel.from, 1), changes.mapPos(sel.to, -1)) };
}

function setLineKind(state: EditorState, kind: LineKind): TransactionSpec {
  const lines = targetLines(state);
  const parsed = lines.map((l) => parseLine(l.text));
  const off = parsed.every((p) => p.kind === kind);
  // a numbered list carries on from the line above
  const before = lines[0].number > 1 ? parseLine(state.doc.line(lines[0].number - 1).text) : null;
  let number = before?.kind === "ordered" ? before.number + 1 : 1;
  const insertFor = (): string => {
    if (off) return "";
    if (kind === "bullet") return "- ";
    if (kind === "task") return "- [ ] ";
    if (kind === "ordered") return `${number++}. `;
    return `${"#".repeat(Number(kind[1]))} `;
  };
  const changes = state.changes(lines.map((l, i) => ({ from: l.from + parsed[i].markerFrom, to: l.from + parsed[i].markerTo, insert: insertFor() })));
  return { changes, selection: state.selection.map(changes, 1) };
}

function toggleQuote(state: EditorState): TransactionSpec {
  const lines = targetLines(state);
  const quoted = lines.map((l) => /^[ \t]*>[ \t]?/.exec(l.text));
  const off = quoted.every(Boolean);
  const changes = state.changes(lines.map((l, i) => (off ? { from: l.from, to: l.from + quoted[i]![0].length } : { from: l.from, insert: "> " })));
  return { changes, selection: state.selection.map(changes, 1) };
}

function toggleCodeBlock(state: EditorState): TransactionSpec {
  const sel = state.selection.main;
  const fenced = selected(state, "FencedCode");
  if (fenced) {
    const first = state.doc.lineAt(fenced.from);
    const last = state.doc.lineAt(fenced.to);
    const closed = last.number > first.number && fenced.lastChild?.name === "CodeMark";
    const specs: ChangeSpec[] = [];
    if (closed && last.number === first.number + 1) specs.push({ from: first.from, to: Math.min(last.to + 1, state.doc.length) });
    else {
      specs.push({ from: first.from, to: Math.min(first.to + 1, state.doc.length) });
      if (closed) specs.push({ from: last.from - 1, to: last.to });
    }
    const changes = state.changes(specs);
    return { changes, selection: state.selection.map(changes) };
  }
  const first = state.doc.lineAt(sel.from);
  const last = state.doc.lineAt(sel.to);
  const shift = 4; // the opening fence and its line break all land before the selection
  if (first.number === last.number && !first.text.trim()) {
    return { changes: { from: first.from, to: first.to, insert: "```\n\n```" }, selection: { anchor: first.from + shift } };
  }
  return { changes: [{ from: first.from, insert: "```\n" }, { from: last.to, insert: "\n```" }], selection: EditorSelection.single(sel.anchor + shift, sel.head + shift) };
}

function toggleLink(state: EditorState): TransactionSpec {
  const link = selectedLink(state);
  if (link) {
    const [open, close] = link.getChildren("LinkMark");
    const changes = state.changes([{ from: open.from, to: open.to }, { from: close.from, to: link.to }]);
    return { changes, selection: state.selection.map(changes) };
  }
  let { from, to } = state.selection.main;
  if (from === to) {
    const word = state.wordAt(from);
    if (word) ({ from, to } = word);
  }
  const text = state.doc.sliceString(from, to);
  if (/^(?:https?:\/\/|mailto:)\S*$/.test(text)) {
    const label = "link text";
    return { changes: { from, to, insert: `[${label}](${text})` }, selection: EditorSelection.single(from + 1, from + 1 + label.length) };
  }
  // The part still to be filled in is left selected, so typing or pasting replaces it.
  if (!text) {
    const label = "link text";
    return { changes: { from, insert: `[${label}](${LINK_PLACEHOLDER})` }, selection: EditorSelection.single(from + 1, from + 1 + label.length) };
  }
  const url = from + text.length + 3;
  return { changes: { from, to, insert: `[${text}](${LINK_PLACEHOLDER})` }, selection: EditorSelection.single(url, url + LINK_PLACEHOLDER.length) };
}

/**
 * Puts a block on lines of its own, with the blank line before it that stops `---` from turning
 * the paragraph above into a heading. `select` is a range within `block` to leave selected.
 */
function insertBlock(state: EditorState, block: string, select?: [number, number]): TransactionSpec {
  const line = state.doc.lineAt(state.selection.main.to);
  const blank = !line.text.trim();
  const above = line.number > 1 && !!state.doc.line(line.number - 1).text.trim();
  const prefix = blank ? (above ? "\n" : "") : "\n\n";
  const from = blank ? line.from : line.to;
  const start = from + prefix.length;
  const insert = `${prefix}${block}\n`;
  const selection = select ? EditorSelection.single(start + select[0], start + select[1]) : EditorSelection.single(start + block.length + 1);
  return { changes: { from, to: blank ? line.to : from, insert }, selection };
}

/** The transaction for a toolbar command, or null if it has nothing to act on. */
export function formatTransaction(state: EditorState, action: FormatAction): TransactionSpec | null {
  switch (action) {
    case "bold":
    case "italic":
    case "strike":
    case "code":
    case "math":
      return toggleInline(state, action);
    case "h1":
    case "h2":
    case "h3":
    case "bullet":
    case "ordered":
    case "task":
      return setLineKind(state, action);
    case "quote":
      return toggleQuote(state);
    case "codeBlock":
      return toggleCodeBlock(state);
    case "link":
      return toggleLink(state);
    case "rule":
      return insertBlock(state, "---");
    case "table":
      return insertBlock(state, TABLE, [2, 10]);
  }
}

/** The commands that are "on" at the selection, for the toolbar to light up. */
export function activeFormats(state: EditorState): FormatAction[] {
  const on = (Object.keys(INLINE) as InlineAction[]).filter((a) => selected(state, INLINE[a].node)) as FormatAction[];
  const first = state.doc.lineAt(state.selection.main.from);
  const kind = parseLine(first.text).kind;
  if (kind && kind !== "h4" && kind !== "h5" && kind !== "h6") on.push(kind);
  if (/^[ \t]*>/.test(first.text)) on.push("quote");
  if (selected(state, "FencedCode")) on.push("codeBlock");
  if (selectedLink(state)) on.push("link");
  return on;
}

/** Flips the task marker whose `[` is at `pos`, or returns null if there isn't one there. */
export function taskToggleAt(state: EditorState, pos: number): TransactionSpec | null {
  const m = /^\[([ xX\t])\]$/.exec(state.doc.sliceString(pos, pos + 3));
  if (!m) return null;
  return { changes: { from: pos + 1, to: pos + 2, insert: m[1] === " " || m[1] === "\t" ? "x" : " " } };
}
