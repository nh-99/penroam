# Importing, exporting and printing

[Manual](README.md) › Importing, exporting and printing

## Moving notebooks between devices

Use **Export and print → Notebook (.penroam, editable)…** in a notebook's top bar to save it as a
file, or **This page (.penroam, editable)…** for a single page. On the other device, click
**Import** in the library. Everything comes across exactly as you left it: ink, text, PDFs and page
order.

To keep notebooks up to date on several devices without moving files, see
[Sync and vaults](sync-and-vaults.md).

## Importing from other apps

Click **Import** in the library and choose one or more files. Each one becomes a new notebook, so
nothing you already have changes. Penroam reads:

* `.penroam` notebooks
* PDFs
* Xournal++ (`.xopp`) and Xournal (`.xoj`) documents
* Evernote exports (`.enex`)
* Joplin exports (`.jex`)
* Markdown and plain text

Other formats never come across perfectly, so after importing, Penroam lists what it kept and what
it couldn't bring. For example:

* A Xournal++ document keeps a PDF stored inside the file, but one kept elsewhere on your disk
  arrives without it, because Penroam won't open a file just because a document names it. Bring
  that PDF in with [Import PDF as pages](pdfs.md).
* LaTeX from Xournal++ comes in as formulas you can edit.
* PDFs attached to Evernote and Joplin notes become pages after their note.
* Markdown image links aren't followed.

## Other ways to export

The **Export and print** menu in a notebook's top bar also has:

* **Annotated PDF…**: the notebook with your ink as sharp vector lines. See [PDFs](pdfs.md).
* **Markdown (text only)…** and **Plain text (text only)…**: your typed words and whatever search
  has read from your handwriting, which may contain mistakes. Handwriting it hasn't read yet is
  left out, and Penroam tells you how much.
* **This page as PNG…** or **SVG…**: a picture of the page. PNG keeps a PDF behind the page; SVG
  doesn't, and shows formulas as their LaTeX source.
* **Copy this page's text**: puts the typed text on the clipboard.

Only a `.penroam` file or a [backup](backups-and-history.md#backing-up-everything) can be edited
again as it was. A PDF, image or text export is a copy to read, not a backup. If you use sync,
Penroam reminds you before any export that the file won't be encrypted the way your synced notes
are; you can go ahead, and can turn the reminder off.

## Printing

Press `Ctrl+P` in a notebook, or choose **Print…** from the **Export and print** menu in the top bar
or from a notebook's **⋮** menu in the library. Your system's print dialog opens, so you choose the
printer, the number of copies and which pages to print there. A printout matches the annotated PDF
export, and Penroam tells you if anything couldn't be included. On Windows and macOS the notebook
opens in your default PDF viewer instead, and you print from there.

When you print, your device's print system receives a copy of what you print.

## Related

* [The `.penroam` file format](../formats/portability.md), documented so your notes are never
  locked into Penroam
