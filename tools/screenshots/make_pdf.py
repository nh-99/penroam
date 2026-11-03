#!/usr/bin/env python3
"""Writes a small two-page text PDF (US Letter, Times) used as the annotated
PDF in the README screenshots. Text positions are fixed so the demo library
generator can place highlights and margin notes on known lines.

    python3 tools/screenshots/make_pdf.py out.pdf
"""
import sys

TITLE = "Spaced Repetition: Notes on Remembering"
PAGES = [
    [
        ("h", "Spaced Repetition: Notes on Remembering"),
        ("s", "A short reading for the study-skills seminar"),
        ("p", ""),
        ("b", "1. The forgetting curve"),
        ("p", "Memories fade quickly after we first learn something. Without review, most of a"),
        ("p", "new lecture is gone within a few days. The decline is steep at first and then"),
        ("p", "levels off: whatever survives the first week tends to stay much longer."),
        ("p", ""),
        ("p", "Each time we successfully recall an idea, the curve flattens. The next review can"),
        ("p", "therefore wait longer than the last one did. This is the core of spaced repetition:"),
        ("p", "review just before you would forget, and let the gaps grow."),
        ("p", ""),
        ("b", "2. Why testing beats rereading"),
        ("p", "Rereading feels productive because the material looks familiar. Familiarity is"),
        ("p", "not the same as recall. Closing the book and writing down what you remember is"),
        ("p", "harder, and that effort is exactly what strengthens the memory."),
        ("p", ""),
        ("p", "A practical routine: after each class, write three questions from memory. Answer"),
        ("p", "them the next day, then three days later, then a week later. Move a question to"),
        ("p", "a longer interval when you get it right and back to tomorrow when you miss it."),
        ("p", ""),
        ("b", "3. Mixing topics"),
        ("p", "Interleaving different kinds of problems in one session feels slower than doing"),
        ("p", "twenty of the same kind in a row, but it trains you to recognise which method a"),
        ("p", "problem needs. That recognition is what exams actually test."),
    ],
    [
        ("b", "4. Sleep and spacing"),
        ("p", "Memory consolidation continues during sleep. Two short sessions on different days"),
        ("p", "usually beat one long session of the same total length."),
        ("p", ""),
        ("b", "Summary"),
        ("p", "Review at growing intervals, test yourself instead of rereading, mix problem types,"),
        ("p", "and spread work across days."),
    ],
]

STYLE = {"h": ("F2", 20, 30), "s": ("F3", 12, 22), "b": ("F2", 13, 22), "p": ("F1", 11, 16)}
LEFT, TOP = 72, 720


def esc(t):
    return t.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def page_stream(lines):
    y, out = TOP, ["BT"]
    for kind, text in lines:
        font, size, lead = STYLE[kind]
        y -= lead
        if text:
            out.append(f"/{font} {size} Tf 1 0 0 1 {LEFT} {y} Tm ({esc(text)}) Tj")
    out.append("ET")
    return "\n".join(out).encode()


def main(path):
    objs = []
    def add(b):
        objs.append(b if isinstance(b, bytes) else b.encode())
        return len(objs)
    fonts = {"F1": "Times-Roman", "F2": "Times-Bold", "F3": "Times-Italic"}
    font_ids = {k: add(f"<< /Type /Font /Subtype /Type1 /BaseFont /{v} >>") for k, v in fonts.items()}
    res = "<< /Font << " + " ".join(f"/{k} {i} 0 R" for k, i in font_ids.items()) + " >> >>"
    pages_id = len(objs) + 1 + 2 * len(PAGES)
    kids = []
    for lines in PAGES:
        s = page_stream(lines)
        sid = add(b"<< /Length %d >>\nstream\n" % len(s) + s + b"\nendstream")
        kids.append(add(f"<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 612 792] /Resources {res} /Contents {sid} 0 R >>"))
    assert add(f"<< /Type /Pages /Kids [{' '.join(f'{k} 0 R' for k in kids)}] /Count {len(kids)} >>") == pages_id
    info = add(f"<< /Title ({esc(TITLE)}) /Producer (penroam tools/screenshots) >>")
    root = add(f"<< /Type /Catalog /Pages {pages_id} 0 R >>")
    out, offs = bytearray(b"%PDF-1.4\n"), []
    for i, b in enumerate(objs, 1):
        offs.append(len(out))
        out += b"%d 0 obj\n" % i + b + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    out += b"".join(b"%010d 00000 n \n" % o for o in offs)
    out += b"trailer\n<< /Size %d /Root %d 0 R /Info %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, root, info, xref)
    open(path, "wb").write(out)


if __name__ == "__main__":
    main(sys.argv[1])
