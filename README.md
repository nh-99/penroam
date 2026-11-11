<h1><img src="docs/images/logo.png" alt="" width="48" height="48" align="absmiddle"> Penroam</h1>

**A handwriting-first notebook that keeps your notes on your own device.**

Penroam is a free, open-source notebook app for people who think with a pen. Write by hand with
a stylus, mouse or finger, type next to your ink, mark up PDFs, and find any of it again later,
including the words you wrote by hand. It works fully offline. You don't need an account, a
server or a subscription.

![A notebook page with handwritten notes, a highlighted formula, a sketched diagram and a typed note](docs/images/editor.png)

## Why Penroam

- **It feels like writing on paper.** Pressure-sensitive ink, a highlighter and an eraser,
  on ruled, grid, dotted or blank pages, or on a canvas with no edges.
- **It does what paper can't.** Typed notes with formatting, formulas and working
  spreadsheets sit on the same page as your handwriting, and search reads your handwriting.
- **Your notes stay yours.** They live on your device in an open, documented file format.
  There are no ads, no analytics and no telemetry, and your notes leave your device only
  when you export, back up or sync them.
- **You won't lose work.** Every stroke is saved as you write, every page keeps its earlier
  versions, and your whole library backs up to a single file.

## What you can do with it

### Write naturally

Choose a pen, highlighter or eraser and write the way you would on paper. Ink responds to
pressure, your stylus's eraser end and side button erase, and a ring shows where a hovering pen
will land. On a touchscreen you decide whether fingers draw or move the page. On BOOX e-ink
tablets, ink is drawn by the device's own low-latency pen layer.

More in [Writing and drawing](docs/manual/writing.md).

### Type, format and calculate on the same page

![A pendulum lab write-up: a handwritten title, a typed heading, a formula, a table of measurements with computed results and checkboxes, and a hand-drawn pendulum](docs/images/spreadsheet.png)

Type anywhere on a page, or on the rows of ruled and dotted paper, and move, resize and recolour
text alongside your ink. Text can be in a handwritten font or a typeset one. Text boxes
understand Markdown (headings, lists, task lists, quotes, code and links) and LaTeX formulas,
which look the same on screen, in exported PDFs and on paper.

Every table is a small spreadsheet: write `=SUM(B2:B5)` or `=ROUND(AVERAGE(C2:C5), 2)`, tick
checkboxes, format money and percentages, and sort and filter rows. Tables are saved as plain
Markdown, so they still read well in any other app.

More in [Typing and formatting text](docs/manual/text.md) and [Spreadsheets](docs/manual/spreadsheets.md).

### Mark up PDFs

![A PDF article with highlights and handwritten margin notes](docs/images/pdf-annotation.png)

Import a PDF as pages, then highlight, underline and write in the margins. Export it again as an
annotated PDF in which your ink stays sharp vector lines. The original PDF is never changed.

More in [PDFs](docs/manual/pdfs.md).

### Think without edges

![A mind map of science fair ideas spread across an infinite dotted canvas](docs/images/infinite-canvas.png)

Turn any notebook into an infinite canvas for mind maps, sketches and brainstorms. Blank, grid
and dotted canvases grow in every direction as you write; ruled paper keeps its width and never
runs out of lines. You can switch a notebook between pages and canvas at any time.

More in [Pages and the infinite canvas](docs/manual/pages-and-canvas.md).

### Find anything, even handwriting

![Search results for "questions": a handwritten heading, a typed note and a passage of an imported PDF](docs/images/search.png)

One search covers every notebook's titles, typed text, PDF text and handwriting. Handwriting
recognition runs on your device, never in the cloud. If a word was read wrongly, correct it once
and search remembers.

More in [Searching](docs/manual/search.md).

### Keep your library in order

![The library showing six notebooks with covers and page previews](docs/images/library.png)

Give each notebook a title, a cover colour, a paper size and a page style, and group notebooks
in folders. Deleted notebooks and pages wait in the trash until you delete them for good. Bring
notes in from Xournal++, Evernote, Joplin, Markdown, text files and PDFs; Penroam tells you
exactly what came across and what didn't.

More in [Your library](docs/manual/library.md) and [Importing](docs/manual/import-export.md#importing-from-other-apps).

### Never lose a thought

There is no save button. Every change is written to disk as you make it, and if the app or your
computer crashes, your notebook reopens with everything you'd written. Version history lets you
look back at any page and restore it, or a whole notebook, without losing what you have now.
Back up your library to a single file whenever you like, or let Penroam back it up every day to
a folder or a WebDAV server such as Nextcloud.

More in [Saving, backups and version history](docs/manual/backups-and-history.md).

### Share between your devices, privately

Notebooks are ordinary `.penroam` files that you can copy to another device. If you'd rather
not, optional sync keeps your notebooks up to date on all your devices. For notes no one else
should ever read, put them in a vault: it's encrypted with a passphrase that only you know.

More in [Sync and vaults](docs/manual/sync-and-vaults.md).

### Print and export

Print a whole notebook or a few pages, with ink as sharp as on screen. Export notebooks as
`.penroam` files, annotated PDFs, PNG or SVG images, Markdown or plain text.

More in [Importing, exporting and printing](docs/manual/import-export.md).

### Make it yours

| Dark theme | ePaper theme |
|---|---|
| ![A packing list with checkboxes, a typed itinerary and a sketch, on black paper in the dark theme](docs/images/dark-theme.png) | ![The same notebook in the high-contrast ePaper theme](docs/images/epaper-theme.png) |

Choose a light, dark or high-contrast ePaper theme. The dark theme shows pages as black paper with
black and white ink swapped, while exports and printouts stay as you wrote them. Pick your own
pen and highlighter colours, and use Penroam on a desktop, a tablet or a phone: the layout adapts
to each.

More in [Settings](docs/manual/settings.md).

## Install

Penroam hasn't published ready-made installers yet, so for now you build it from source.
It has been tested on **Linux** and on **Android BOOX** e-ink tablets. Windows, macOS and
iPad versions build from the same code but haven't been tested on those systems yet. The
[release matrix](docs/platforms/release-matrix.md) has the status of each platform.

### Linux

You need [Rust](https://rustup.rs), [Node.js](https://nodejs.org) 24 with
[pnpm](https://pnpm.io) 10, and the WebKitGTK development package (on Debian/Ubuntu:
`sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev librsvg2-dev`).

```bash
git clone https://github.com/nh-99/penroam.git
cd penroam
pnpm install
pnpm tauri build --no-bundle
./target/release/penroam-app
```

`pnpm tauri build` also makes an AppImage in `target/release/bundle/appimage/`. When it starts,
the AppImage adds Penroam to your app menu and keeps the entry pointing at it if you move the file.
A build you make yourself is listed as **Penroam Developer**, so it doesn't replace an official
release you've also installed.
To uninstall, choose **Remove from app menu** in **Settings → Advanced**, then delete the AppImage.

### Android and BOOX

Building the Android app needs the Android SDK and NDK. See
[docs/development.md](docs/development.md) for the steps. Then install the APK with
`adb install`.

## Using Penroam

The first time you open Penroam, a short guided tour shows you around. You can take it again at any
time from **Settings → General**.

1. Click **New notebook**, give it a title and choose the paper.
2. Pick the pen (`P`) and write. The highlighter is `M`, the eraser `E` and the text tool `T`.
3. Add pages with **+**, and import a PDF with the button next to it.
4. Find anything with the magnifying glass (`Ctrl+K`).

There is no save button: everything is saved as you write.

The **[user manual](docs/manual/README.md)** explains everything else:

* [Getting started](docs/manual/getting-started.md)
* [Your library](docs/manual/library.md): notebooks, folders and the trash
* [Writing and drawing](docs/manual/writing.md): tools, colours, pens and touch
* [Typing and formatting text](docs/manual/text.md): Markdown, formulas and links
* [Spreadsheets](docs/manual/spreadsheets.md)
* [Pages and the infinite canvas](docs/manual/pages-and-canvas.md)
* [PDFs](docs/manual/pdfs.md)
* [Searching](docs/manual/search.md), including handwriting
* [Importing, exporting and printing](docs/manual/import-export.md)
* [Saving, backups and version history](docs/manual/backups-and-history.md)
* [Sync and vaults](docs/manual/sync-and-vaults.md)
* [Settings](docs/manual/settings.md)
* [Keyboard shortcuts](docs/manual/shortcuts.md)

## Where your notes live

Notebooks are stored in the app's private data folder on your device. They leave it only when
you export them, back them up, or turn on sync. You don't need an account, and Penroam has no
analytics. Search works entirely on your device: the
search index is kept in the app's cache folder, and neither it nor anything you search for is
uploaded or included in exports. When you print, your device's print system receives a copy of
what you print. The `.penroam` format is documented
in [docs/formats/](docs/formats/), so your notes are never locked into this app.

## Contributing

Penroam is free software, and contributions are welcome. The
[developer README](docs/README.md) covers setting up a development environment, building,
testing and how the code fits together. See also [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Penroam is licensed under the [MPL-2.0](https://www.mozilla.org/MPL/2.0/).
The bundled Liberation Sans and Excalifont fonts are under the SIL Open Font License 1.1, and PDF.js is under
Apache-2.0. The Android build fetches the proprietary BOOX Onyx SDK from Onyx's official
repository to support BOOX pens.

## Help and privacy

**Problems.** See [getting help](docs/support.md) and [known issues](docs/known-issues.md). If
Penroam crashes it keeps a short report on your device, and nothing is sent anywhere; see
[Settings → About](docs/manual/settings.md#about) to share it in a bug report.

**Updates.** Builds that can update themselves check for new versions only if you turn that on in
**Settings → About**. See [Settings](docs/manual/settings.md#about).

**Privacy.** Penroam has no analytics or telemetry. [Privacy](docs/policies/privacy.md) lists every
case in which the app uses the network. See also [accessibility](docs/policies/accessibility.md).

**Security.** See [SECURITY.md](SECURITY.md).
