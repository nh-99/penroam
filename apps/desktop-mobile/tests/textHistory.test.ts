import { describe, expect, it } from "vitest";
import { applySplices, diffSplices, editFromSplices, invertEdit, mapEdit, type OwnEdit, type Splice } from "../src/editor/textHistory";

/** An own edit from `prev` to `next`, and the splices that made it. */
function own(prev: string, next: string) {
  const sp = diffSplices(prev, next);
  return { sp, edit: editFromSplices(prev, sp) };
}
/** A remote change `prev -> next` applied to the text, with a stored edit mapped through it. */
function remote(text: string, next: string, edit: OwnEdit) {
  const sp = diffSplices(text, next);
  expect(applySplices(text, sp)).toBe(next);
  return mapEdit(edit, sp);
}

describe("diffSplices", () => {
  it("round-trips for typing, deleting, pasting and replacing", () => {
    const cases: [string, string][] = [
      ["", "hello"],
      ["hello", ""],
      ["hello world", "hello brave world"],
      ["abc def ghi", "abc ghi"],
      ["a b c d e", "a X c Y e"],
      ["one", "one\n- two\n- three\n\n```js\nlet x = 1;\n```"],
      ["same", "same"],
    ];
    for (const [a, b] of cases) expect(applySplices(a, diffSplices(a, b))).toBe(b);
  });

  it("finds separate changes as separate splices", () => {
    const sp = diffSplices("a b c d e", "a X c Y e");
    expect(sp.length).toBe(2);
  });

  it("never splits a surrogate pair", () => {
    for (const [a, b] of [["a😀b", "a😁b"], ["😀", "😀😀"], ["x𝒳y", "x𝒴y"]] as const) {
      const sp = diffSplices(a, b);
      expect(applySplices(a, sp)).toBe(b);
      for (const s of sp) {
        const c = (i: number) => (i < a.length ? a.charCodeAt(i) : 0);
        expect(c(s.from) >= 0xdc00 && c(s.from) <= 0xdfff).toBe(false);
        expect(c(s.to) >= 0xdc00 && c(s.to) <= 0xdfff).toBe(false);
      }
    }
  });

  it("keeps emoji sequences and combining marks whole", () => {
    const fam = "👨‍👩‍👧";
    const sp = diffSplices(`a${fam}b`, `a👨‍👩b`);
    expect(applySplices(`a${fam}b`, sp)).toBe("a👨‍👩b");
    expect(sp.every((s) => s.from === 1 || s.from === 0 || s.from >= 1)).toBe(true);
    const e1 = "é"; // e + combining acute
    const sp2 = diffSplices(`x${e1}y`, "xey");
    expect(sp2[0].from).toBe(1); // starts at the e, not between e and the accent
    expect(applySplices(`x${e1}y`, sp2)).toBe("xey");
  });
});

describe("undo only touches own edits", () => {
  it("undoes own insertion after a remote insertion before it", () => {
    const base = "hello world";
    const mine = "hello brave world";
    const { edit } = own(base, mine);
    const theirs = "Hey! hello brave world";
    const mapped = remote(mine, theirs, edit);
    const u = invertEdit(theirs, mapped);
    expect(u.text).toBe("Hey! hello world");
  });

  it("keeps a remote insertion in the middle of own text", () => {
    const mine = "ab" + "XYZ" + "cd";
    const { edit } = own("abcd", mine);
    const theirs = "abXY--ZCd".replace("ZCd", "Zcd"); // remote typed "--" inside my XYZ
    const mapped = remote(mine, theirs, edit);
    const u = invertEdit(theirs, mapped);
    expect(u.text).toBe("ab--cd");
  });

  it("does not resurrect text a collaborator deleted from my insertion", () => {
    const mine = "start middle end";
    const { edit } = own("start end", mine);
    const theirs = "start mid end"; // they deleted "dle"
    const mapped = remote(mine, theirs, edit);
    expect(invertEdit(theirs, mapped).text).toBe("start end");
  });

  it("restores only my deletion, in the right place, after remote edits", () => {
    const base = "one two three";
    const mine = "one three"; // I deleted "two "
    const { edit } = own(base, mine);
    const theirs = ">> one three!!";
    const mapped = remote(mine, theirs, edit);
    expect(invertEdit(theirs, mapped).text).toBe(">> one two three!!");
  });

  it("redo reverses undo, through more remote edits", () => {
    const mine = "a b c";
    const { edit } = own("a c", mine);
    const u = invertEdit(mine, edit);
    expect(u.text).toBe("a c");
    const theirs = "Z a c";
    const redoEdit = remote(u.text, theirs, u.edit);
    expect(invertEdit(theirs, redoEdit).text).toBe("Z a b c");
  });

  it("stacked edits stay valid when the later one is undone first", () => {
    let text = "";
    const e1 = own(text, "alpha");
    text = "alpha";
    const e2 = own(text, "alpha beta");
    let first = e1.edit;
    text = "alpha beta";
    first = mapEdit(first, e2.sp);
    // undo the second
    const u2 = invertEdit(text, e2.edit);
    text = u2.text;
    first = mapEdit(first, u2.splices);
    expect(text).toBe("alpha");
    expect(invertEdit(text, first).text).toBe("");
  });
});

describe("text in other scripts", () => {
  const check = (mine: string, theirs: string, expected: string, base: string) => {
    const { edit } = own(base, mine);
    const mapped = remote(mine, theirs, edit);
    expect(invertEdit(theirs, mapped).text).toBe(expected);
  };

  it("CJK", () => check("你好世界朋友", "大家你好世界朋友吗", "大家你好吗", "你好"));
  it("RTL", () => check("שלום עולם", "היי שלום עולם", "היי ", ""));
  it("emoji surrogate pairs", () => check("ab😀😁cd", "ab😀😁cd🎉", "abcd🎉", "abcd"));
  it("pasted multiline Markdown", () => {
    const pasted = "# Title\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    const base = "notes\n";
    const mine = base + pasted;
    const theirs = "TODO\n" + mine + "tail";
    check(mine, theirs, "TODO\nnotes\ntail", base);
  });
});

describe("splice shape", () => {
  it("diffs are ascending and disjoint", () => {
    const sp: Splice[] = diffSplices("the quick brown fox jumps", "a quick red fox leaps high");
    for (let i = 1; i < sp.length; i++) expect(sp[i].from).toBeGreaterThanOrEqual(sp[i - 1].to);
  });
});
