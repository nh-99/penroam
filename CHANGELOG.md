# Changelog

User-visible changes for each release. The release workflow copies a version's section into its
release notes and stops if the version has no entry.

## 0.1.0-beta.9

* New: duplicate a notebook with **Duplicate** in the **⋮** menu on its card. The copy is called
  "*title* (copy)" and goes in the same folder, or the same vault. It has every page and
  attachment, but starts with an empty trash and no version history.

## 0.1.0-beta.8

* Fixed: in the dark theme, text you were typing or editing showed black on the black paper.
  It now shows in the same colours as the rest of the page.
* Fixed: in the dark theme, selected text and a table's selected cells were tinted almost black.
  They now get a lighter tint that stands out on the black paper.

## 0.1.0-beta.7

* New: infinite canvases. A canvas is a page with no edges: write, type and pan in any direction
  and it grows with you, on blank, grid or dotted paper. Ruled paper keeps the page's width,
  header and margin line and goes on downward for as long as you write. Tick **Infinite canvas**
  when you create a notebook, or use **Page template → Infinite canvas** to switch a notebook
  either way. Turning it off shows the pages as sheets again without changing anything on them.
* A canvas notebook can still have several pages, each a canvas of its own. The page sidebar
  starts closed there. When you turn the canvas on for a notebook with several pages, you can keep
  them or combine them onto one canvas, one below the other; the emptied pages go to the
  notebook's trash.
* On a canvas, **Import PDF onto this canvas** puts a whole PDF on the open page, below what's
  already there. Its text is searchable, and **Undo** takes it off again.
* Annotated PDF export and printing lay a canvas out on pages of the notebook's paper size,
  breaking between lines where they can; on ruled paper only the first page has the header. PNG
  and SVG export give one picture of the whole canvas.
* Devices still on an older version show canvas notebooks as ordinary pages, and ask to be updated
  once a notebook is switched to or from a canvas.
* New: spreadsheets in notes. A table in a text box becomes a grid while you edit: type into
  cells, move with `Tab`, `Enter` and the arrow keys, fill down with `Ctrl+D`, and copy and paste
  blocks of cells to and from other spreadsheet apps. Tables keep the box's font, so handwritten
  tables stay handwritten, and they are still saved as ordinary Markdown tables.
* Cells starting with `=` are formulas (`=B2*C2`, `=SUM(B2:B5)`, with `AVERAGE`, `MIN`, `MAX`,
  `COUNT`, `PRODUCT`, `ROUND` and `ABS`), and the page and PDF exports show their results. Click a
  cell while writing a formula to put in its reference. `[ ]` and `[x]` cells are checkboxes you
  can tick in the grid or on the page.
* Numbers can be money (`$9.99`, `$ 9.99`, `9.99 €`, `($9.99)` for a negative amount, in `$`,
  `€`, `£` or `¥`), percentages or plain numbers with thousands separators, and dates are
  `2026-11-10` or `11/10/2026`.
* Each column header has a hand-drawn sort arrow: tap it to sort by that column, and again to sort
  the other way. Sorting understands numbers, money, dates and checkboxes, and keeps a totals row
  in place. The menu next to the arrow aligns, formats, inserts and deletes columns, and the
  filter row hides rows by text, numbers, dates or checkboxes while you work.
* The bar above a table formats the selected cells: bold, italic, strikethrough, currency,
  percentage, thousands separators, decimals and alignment. The main toolbar's bold, italic and
  strikethrough buttons format cells too, and a bold number still counts as a number.
* Devices still on an older version show a table's formulas as written, not their results.
* New: handwritten text. Text boxes can use a handwritten font (Excalifont) or the formatted font
  used until now, picked with the two **Aa** buttons in the toolbar for new text or for selected
  text boxes. New text is handwritten by default; existing text stays formatted. Bullets,
  numbered lists, checkboxes (sketched by hand in handwritten text), links, bold and italic all
  work in both, and PDF and SVG exports include the handwritten font. Older versions show handwritten text in the formatted font.
* The dark theme now shows pages dark too: the paper and PDF pages are inverted, and black, white
  and grey ink and text swap, so black writing shows white on black paper. Coloured ink and
  pictures keep their colours. Only the screen changes: exports, printing and page thumbnails
  stay as written.

## 0.1.0-beta.6

0.1.0-beta.5 reached TestFlight and Google Play testing but never got a GitHub release, so if you
install from GitHub, its changes below are new to you as well.

* Pages scroll continuously: scroll past the end of a page and the next one follows, and a press
  on another page opens it. The switch at the bottom right of the canvas goes back to one page at a
  time.
* New: folders in the library. Make one with the folder button at the top, then use a notebook's
  **⋮** menu and **Move to folder…**. Folders can hold folders and sync with your account. Deleting
  a folder moves what's inside it up one level. Devices still on an older version keep the folder
  changes and ask to be updated.
* Text boxes have formatting buttons while you edit (bold, italic, headings, lists, checkboxes,
  links, tables and more), and `Ctrl+B`, `Ctrl+I` and `Ctrl+K` work. Formatting shows as you type,
  with the Markdown symbols only on the line you're on.
* Tap a checkbox in a task list to tick or untick it.
* The palette button after the toolbar colours picks any colour, and **Settings → General → Tool
  colours** changes which colours the toolbar offers.
* When the toolbar doesn't fit the window, arrows at each end move along it, and so does the mouse
  wheel.

## 0.1.0-beta.5

* The Linux AppImage adds Penroam to your app menu. **Settings → Advanced** can remove it before you
  delete the AppImage.
* Vaults moved from Settings to a row above your notebooks on the library screen, where you create,
  unlock and manage them.
* Vaults can be locked on a device: their notebooks disappear from the library and search and are
  encrypted there until you enter the passphrase again, which works offline. **Settings → Advanced**
  can lock every vault when the app closes.
* Vault notebooks are no longer included in backups.
* **Delete vault** is now **Stop syncing with the cloud**: the cloud copy is erased and the notebooks
  stay on your devices, marked **Not synced**, where you can **Restore cloud sync** or remove them.
* Dialogs have room below their buttons again, and messages no longer sit on the bottom edge of the
  window.
* A long press while drawing no longer selects the text in the top bar and toolbar.
* Penroam no longer crashes on Android when the handwriting model finishes downloading or loads at
  startup.
* If a handwriting model ever crashes Penroam while loading, Penroam stops using it so the app still
  opens, and says so in **Settings → Advanced**.

## 0.1.0-beta.4

The first public test build. It's for testers: expect rough edges, and keep a backup of notebooks
you care about.

* Adding a device now takes a sign-in. There's no pairing code, comparison code or recovery code, and
  a device names itself after your computer or tablet.
* New: vaults. Notebooks in a vault are readable only with your passphrase. Ordinary notebooks are
  encrypted too, but Penroam now holds their key, so Penroam could read them.
