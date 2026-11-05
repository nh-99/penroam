<h1><img src="docs/images/logo.png" alt="" width="48" height="48" align="absmiddle"> Penroam</h1>

**A handwriting-first notebook that keeps your notes on your own device.**

Penroam is a free, open-source notebook app for writing by hand with a stylus, mouse or finger.
You can type text on the same page as your ink, read and annotate PDFs, and carry notebooks
between devices as ordinary `.penroam` files. It works fully offline and doesn't need an
account, a server or a subscription.

![A notebook page with handwritten notes, a highlighted formula, a sketched diagram and a typed note](docs/images/editor.png)

## Highlights

- **Write naturally.** A pressure-sensitive pen, a highlighter and a stroke eraser (including
  your stylus's eraser button). On BOOX e-ink tablets, ink is drawn by the device's own
  low-latency pen layer.
- **Ink and typing together.** Drop a text box anywhere on the page, then move it, resize it,
  recolour it or change its size alongside your handwriting.
- **Annotate PDFs.** Import a PDF as pages, then write and type on it. Export an annotated PDF
  in which your ink stays sharp vector lines. The original PDF is kept unchanged.
- **Paper that suits the note.** Letter or A4 pages, blank, ruled, grid or dotted.
  Add, reorder, delete and restore pages from the sidebar.
- **Never lose work.** Every stroke is saved to disk as you go, and the indicator in the top
  bar tells you when it has been. If the app or computer crashes, your notebook reopens with
  everything you'd written.
- **Your files, your choice.** Export any notebook as a `.penroam` file (a documented,
  lossless format) and open it on another device.
- **Light, dark and ePaper themes**, with layouts for desktop, tablet and phone.

| Your library | Annotating a PDF |
|---|---|
| ![The library showing five notebooks with covers and page previews](docs/images/library.png) | ![A PDF article with highlights and handwritten margin notes](docs/images/pdf-annotation.png) |

| Dark theme | ePaper theme |
|---|---|
| ![A packing list with checkboxes, a typed itinerary and a sketch, in the dark theme](docs/images/dark-theme.png) | ![The same notebook in the high-contrast ePaper theme](docs/images/epaper-theme.png) |

## Install

Penroam hasn't published ready-made installers yet, so for now you build it from source.
It has been tested on **Linux** and on **Android BOOX** e-ink tablets. Windows, macOS and
iPad versions build from the same code but haven't been tested on those systems yet.

### Linux

You need [Rust](https://rustup.rs), [Node.js](https://nodejs.org) 24 with
[pnpm](https://pnpm.io) 10, and the WebKitGTK development package (on Debian/Ubuntu:
`sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev librsvg2-dev`).

```bash
git clone https://github.com/penroam/penroam.git
cd penroam
pnpm install
pnpm tauri build --no-bundle
./target/release/penroam-app
```

### Android and BOOX

Building the Android app needs the Android SDK and NDK. See
[docs/development.md](docs/development.md) for the steps. Then install the APK with
`adb install`.

## Using Penroam

**Notebooks.** Click **New notebook** and choose a title, cover colour, paper size and page
style. To rename, recolour or delete a notebook, open the **⋮** menu on its card. Deleted
notebooks go to the **Trash**, where you can restore them or delete them for good.

**Writing.** Choose a tool from the toolbar, then a colour and thickness:

| Tool | Key | What it does |
|---|---|---|
| Pen | `P` | Pressure-sensitive ink |
| Highlighter | `M` | Translucent marker |
| Eraser | `E` | Removes whole strokes |
| Select | `V` | Drag a box around ink or text to move or delete it |
| Text | `T` | Click anywhere to type. `Esc` finishes the text box |
| Pan | `H` | Move around the page |

Undo and redo with `Ctrl+Z` / `Ctrl+Shift+Z`. Zoom with `Ctrl` and the mouse wheel, a pinch,
or `Ctrl+=` / `Ctrl+-`. `Ctrl+0` fits the page to the window width.

On a touchscreen, **Settings** decides what your fingers do:

- **Automatic** (the default): fingers draw until you use a pen, then they pan and zoom.
- **Pen only**: fingers always pan and zoom.
- **Fingers draw**: one finger draws, two fingers pan and zoom.

**Pages.** The sidebar shows all pages. Use **+** (or `Ctrl+Shift+N`) to add a page, drag
thumbnails to reorder them, and open a page's **⋮** menu to insert, move or delete it.
**Page template** in the top bar changes the current page's style. `PgUp` / `PgDn` move
between pages.

**PDFs.** Click **Import PDF as pages** in the top bar to add every page of a PDF to the
notebook, then write on it like any other page. To get the PDF back with your notes on it,
choose **Export → Annotated PDF**.

**Searching.** Click the magnifying glass (`Ctrl+K` in the library, `Ctrl+F` in a notebook)
and type. Penroam searches every notebook's titles, typed text and the text of imported PDFs.
It also reads your handwriting: on Windows with the handwriting recognizer built into Windows,
and on devices without one with a handwriting model, a file you install under
**Settings → Advanced** (it runs on your device and is used as a fallback on Windows too).
Choose a result to open its page with the matching words outlined. Quoted words
(`"budget review"`) match as a phrase, and close misspellings still find handwriting that was
read slightly wrong (marked *similar spelling*). If a handwritten line was read wrongly, click
the pencil next to the result and type what it says; search uses your reading from then on, and
your ink stays exactly as you wrote it. **Settings → Search** turns handwriting recognition on
or off, pauses indexing, waits while the device runs on battery, and rebuilds or deletes the
search index. Without a recognizer or an installed model, Penroam searches titles, typed text and
PDF text, but not handwriting.

**Settings → Advanced** chooses between your device's recognizer and the Penroam model where you
have both, and installs or removes the model.

**Moving notebooks between devices.** Use **Export → Notebook (.penroam)** to save a
notebook as a file. On the other device, click **Import** in the library. Everything comes
across exactly as you left it: ink, text, PDFs and page order.

**Saving.** There is no save button to remember. Changes are written to disk as you make
them, and the top bar shows **Saved** once they are safe. `Ctrl+S` writes everything
immediately if you want extra reassurance.

## Where your notes live

Notebooks are stored in the app's private data folder on your device and nowhere else.
Penroam has no accounts and no analytics. Search works entirely on your device: the
search index is kept in the app's cache folder, and neither it nor anything you search for is
uploaded or included in exports. The `.penroam` format is documented
in [docs/formats/](docs/formats/), so your notes are never locked into this app.

## Contributing

Developer documentation lives in [docs/](docs/). Start with
[docs/development.md](docs/development.md) for building and testing, and
[docs/architecture/overview.md](docs/architecture/overview.md) for how the app fits together.

## License

Penroam is licensed under the [MPL-2.0](https://www.mozilla.org/MPL/2.0/).
The bundled Liberation Sans font is under the SIL Open Font License 1.1, and PDF.js is under
Apache-2.0. The Android build fetches the proprietary BOOX Onyx SDK from Onyx's official
repository to support BOOX pens.
