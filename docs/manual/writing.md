# Writing and drawing

[Manual](README.md) › Writing and drawing

## Tools

Choose a tool from the toolbar, then a colour and thickness:

| Tool | Key | What it does |
|---|---|---|
| Pen | `P` | Pressure-sensitive ink |
| Highlighter | `M` | Translucent marker |
| Eraser | `E` | Removes whole strokes |
| Select | `V` | Drag a box around ink or text to move or delete it |
| Lasso | `L` | Draw a loop around ink or text to select it, then drag inside the selection to move it |
| Text | `T` | Click where you want to type, click a text box to edit it, or drag one to move it. `Esc` finishes the text box. See [Typing and formatting text](text.md) |
| Pan | `H` | Move around the page. With a finger it scrolls up and down, and a tap on a text box types in it |

Penroam remembers your last tool on each device. On a phone, notebooks always open with the pan
tool, so a finger scrolls and a tap on text starts typing; choose the pen to draw with your finger.

If the toolbar doesn't fit the window, arrows appear at each end to move along it. With a mouse you
can also turn the wheel over the toolbar, or hold the middle button and drag.

## Colours

For a colour that isn't in the toolbar, press the palette button after the colours, then use the
sliders or type a hex value such as `#1d4ed8`. To change which colours the toolbar offers, see
[Tool colours in Settings](settings.md#general).

## Pens

A pen's eraser end erases, and so does holding its side button, including on an S Pen. If your pen
hovers, a ring shows where it will touch down. On an iPad, double-tapping the Apple Pencil does what
you chose in iPadOS Settings: switch to the eraser or to your previous tool. **Settings → General**
shows how pen input works on your device, and lets you turn pressure sensitivity off.

On BOOX e-ink tablets, ink is drawn by the device's own low-latency pen layer. S Pen and desktop pen
support has not been tested on real hardware yet, so tell us how it goes.

## Fingers and touch

Wherever a finger moves the page rather than drawing, one finger scrolls up and down, and the page
keeps going for a moment when you flick it (except in the ePaper theme). To move sideways or zoom,
use two fingers.

On a touchscreen, **Settings → General → Touch input** decides when fingers draw:

* **Automatic** (the default): fingers draw with the drawing tools until you use a pen, then they
  scroll.
* **Pen only**: fingers always scroll.
* **Fingers draw**: one finger draws, two fingers pan and zoom.

With the pan tool, a finger always scrolls.

## Ruler

The ruler button after the tools (or `R`) shows an on-screen ruler for straight lines and exact
angles. It works with the pen and highlighter, and `R` hides it again. It isn't saved in the
notebook; it stays where you left it while you move between pages.

* **Move it:** drag the ruler with a finger or the mouse. It stays put on the screen while you pan
  or zoom the page, like a real ruler lying on the paper.
* **Rotate it:** drag the dial in the middle, or put two fingers on the ruler and turn them (this
  moves it at the same time). The angle clicks into every 15° when you get close, and a label shows
  it in degrees while you turn this way. With the keyboard, `[` and `]` step to the previous or next 15°.
* **Draw along it:** with a pen, start a stroke anywhere on the ruler, or with any input just
  beside either edge, and the whole stroke runs exactly along the nearer edge. A stroke that starts
  further away is never straightened, even if it crosses the ruler later. The pen itself never moves
  the ruler, except when you drag the dial.
* **Measure:** the centimetre marks measure the page, so they match the paper at any zoom.

Fingers move and turn the ruler even in pen-only mode, so you can hold it with a finger while you
draw with the pen. Hiding and showing it again keeps its last position and angle.

With a mouse or finger, pressing on the ruler moves it whatever the tool is. To erase, select or type
under it, move the ruler away first, or use the pen.

## Undo and zoom

Undo and redo with `Ctrl+Z` / `Ctrl+Shift+Z`, or the arrows in the toolbar. Zoom with `Ctrl` and the
mouse wheel, a pinch on a touchpad or touchscreen, `Ctrl+=` / `Ctrl+-`, or the zoom buttons.
`Ctrl+0` fits the page to the window width. On macOS, use `Cmd` instead of `Ctrl`.

## Related

* [Typing and formatting text](text.md)
* [Pages and the infinite canvas](pages-and-canvas.md)
* [Keyboard shortcuts](shortcuts.md)
