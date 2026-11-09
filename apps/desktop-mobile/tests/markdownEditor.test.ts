import { EditorSelection, EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import { activeFormats, type FormatAction, formatTransaction, taskToggleAt } from "../src/editor/markdownFormat";
import { markdownSyntax, previewDecorations } from "../src/editor/markdownPreview";

/** A state from text where `|` is the caret, or `«` and `»` bound the selection. */
function stateOf(marked: string): EditorState {
  const caret = marked.indexOf("|");
  const selection =
    caret >= 0 ? EditorSelection.single(caret) : EditorSelection.single(marked.indexOf("«"), marked.indexOf("»") - 1);
  return EditorState.create({ doc: marked.replace(/[|«»]/g, ""), selection, extensions: [markdownSyntax()] });
}

/** The state's text with the selection marked the same way. */
function marked(state: EditorState): string {
  const doc = state.doc.toString();
  const { from, to, empty } = state.selection.main;
  if (empty) return `${doc.slice(0, from)}|${doc.slice(from)}`;
  return `${doc.slice(0, from)}«${doc.slice(from, to)}»${doc.slice(to)}`;
}

function run(input: string, action: FormatAction): string {
  const state = stateOf(input);
  return marked(state.update(formatTransaction(state, action)!).state);
}

describe("formatting commands", () => {
  it("wraps a selection and keeps the text selected", () => {
    expect(run("a «word» b", "bold")).toBe("a **«word»** b");
    expect(run("a «word» b", "italic")).toBe("a *«word»* b");
    expect(run("a «word» b", "strike")).toBe("a ~~«word»~~ b");
    expect(run("a «word» b", "code")).toBe("a `«word»` b");
    expect(run("a «x+y» b", "math")).toBe("a $«x+y»$ b");
  });

  it("wraps the word at the caret, or starts an empty pair between words", () => {
    expect(run("a wo|rd b", "bold")).toBe("a **wo|rd** b");
    expect(run("a | b", "bold")).toBe("a **|** b");
  });

  it("turns a style off from inside it", () => {
    expect(run("a **wo|rd** b", "bold")).toBe("a wo|rd b");
    expect(run("a **«word»** b", "bold")).toBe("a «word» b");
    expect(run("a *wo|rd* b", "italic")).toBe("a wo|rd b");
    expect(run("a $x|y$ b", "math")).toBe("a x|y b");
  });

  it("leaves spaces and block markers outside a multi-line style", () => {
    expect(run("«- one \n- two»", "bold")).toBe("«- **one** \n- **two»**");
    expect(run("a« word »b", "bold")).toBe("a« **word** »b");
  });

  it("sets, swaps and clears line formats", () => {
    expect(run("ti|tle", "h1")).toBe("# ti|tle");
    expect(run("# ti|tle", "h2")).toBe("## ti|tle");
    expect(run("## ti|tle", "h2")).toBe("ti|tle");
    expect(run("|", "task")).toBe("- [ ] |");
    expect(run("- ite|m", "task")).toBe("- [ ] ite|m");
    expect(run("- [x] ite|m", "bullet")).toBe("- ite|m");
    expect(run("> a|", "h1")).toBe("> # a|");
  });

  it("applies to every selected line, skipping blank ones, and numbers on from the line above", () => {
    expect(run("«a\n\nb»", "bullet")).toBe("- «a\n\n- b»");
    expect(run("«a\nb»", "ordered")).toBe("1. «a\n2. b»");
    expect(run("3. x\n|y", "ordered")).toBe("3. x\n4. |y");
  });

  it("toggles quotes", () => {
    expect(run("«a\nb»", "quote")).toBe("> «a\n> b»");
    expect(run("«> a\n> b»", "quote")).toBe("«a\nb»");
  });

  it("wraps and unwraps code blocks", () => {
    expect(run("|", "codeBlock")).toBe("```\n|\n```");
    expect(run("«a\nb»", "codeBlock")).toBe("```\n«a\nb»\n```");
    expect(run("```\nco|de\n```", "codeBlock")).toBe("co|de");
  });

  it("makes links, leaving the part to fill in selected, and unlinks", () => {
    expect(run("see «docs» now", "link")).toBe("see [docs](«https://») now");
    expect(run("see | now", "link")).toBe("see [«link text»](https://) now");
    expect(run("«https://a.example»", "link")).toBe("[«link text»](https://a.example)");
    expect(run("[do|cs](https://a.example)", "link")).toBe("do|cs");
  });

  it("puts rules and tables on their own lines, so `---` can't make a heading", () => {
    expect(run("|", "rule")).toBe("---\n|");
    expect(run("text|", "rule")).toBe("text\n\n---\n|");
    expect(run("text\n|", "rule")).toBe("text\n\n---\n|");
    expect(run("|", "table")).toBe("| «Column 1» | Column 2 |\n| --- | --- |\n|  |  |\n");
  });

  it("reports what is on at the caret", () => {
    expect(activeFormats(stateOf("**a|b**"))).toEqual(["bold"]);
    expect(activeFormats(stateOf("***a|b***")).sort()).toEqual(["bold", "italic"]);
    expect(activeFormats(stateOf("# a|"))).toEqual(["h1"]);
    expect(activeFormats(stateOf("> - [ ] a|")).sort()).toEqual(["quote", "task"]);
    expect(activeFormats(stateOf("```\na|\n```"))).toEqual(["codeBlock"]);
    expect(activeFormats(stateOf("[a|](https://b)"))).toEqual(["link"]);
    expect(activeFormats(stateOf("a|"))).toEqual([]);
  });

  it("ticks and unticks a task marker, and refuses anything else", () => {
    const tick = (doc: string, pos: number) => stateOf(`${doc}|`).update(taskToggleAt(stateOf(`${doc}|`), pos)!).state.doc.toString();
    expect(tick("- [ ] a", 2)).toBe("- [x] a");
    expect(tick("- [X] a", 2)).toBe("- [ ] a");
    expect(taskToggleAt(stateOf("- plain|"), 2)).toBeNull();
  });
});

describe("live preview", () => {
  /** What is decorated: `class:text` for marks and lines, `hide:text` for hidden source. */
  function decorated(input: string): string[] {
    const state = stateOf(input);
    const out: string[] = [];
    previewDecorations(state).between(0, state.doc.length, (from, to, d) => {
      const cls = (d.spec.class as string | undefined) ?? (d.spec.widget ? "widget" : "hide");
      out.push(`${cls}:${state.doc.sliceString(from, to)}`);
    });
    return out;
  }

  it("styles text on the caret's line and shows its markers there", () => {
    const d = decorated("a **b|** c");
    expect(d).toContain("cm-md-strong:**b**");
    expect(d).toContain("cm-md-syntax:**");
    expect(d.some((x) => x.startsWith("hide:"))).toBe(false);
  });

  it("hides markers on other lines", () => {
    const d = decorated("a **b** c\n|");
    expect(d).toContain("cm-md-strong:**b**");
    expect(d.filter((x) => x === "hide:**")).toHaveLength(2);
  });

  it("hides a heading's marker with its space, and styles the line", () => {
    const d = decorated("# Title\n|");
    expect(d).toContain("hide:# ");
    expect(d).toContain("cm-md-h1:");
  });

  it("draws checkboxes and bullets away from the caret, and leaves numbers", () => {
    expect(decorated("- [ ] a\n- b\n1. c\n\n|").filter((x) => x.startsWith("widget"))).toEqual(["widget:[ ] ", "widget:- "]);
    expect(decorated("- [ ] a|").some((x) => x.startsWith("widget"))).toBe(false);
  });

  it("shows a link's text and hides its address", () => {
    const d = decorated("[text](https://a.example)\n|");
    expect(d).toContain("cm-md-link:text");
    expect(d).toContain("hide:[");
    expect(d).toContain("hide:](https://a.example)");
  });

  it("leaves a bare bracket alone", () => {
    expect(decorated("[text]\n|")).toEqual([]);
  });

  it("treats a formula as one unit, so its asterisks aren't emphasis", () => {
    const d = decorated("$a*b*c$\n|");
    expect(d).toEqual(["cm-md-math:$a*b*c$"]);
  });

  it("doesn't format inside code", () => {
    expect(decorated("```\n**a**\n```\n|").some((x) => x.includes("strong"))).toBe(false);
    expect(decorated("`**a**`\n|").some((x) => x.includes("strong"))).toBe(false);
  });
});
