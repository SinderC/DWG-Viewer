# Using the viewer

Open the [live app](https://sinderc.github.io/DWG-Viewer/), then open a file with the Open button,
<kbd>Ctrl</kbd>/<kbd>⌘</kbd>+<kbd>O</kbd>, or by dropping it on the page. Files are read locally and
never uploaded.

**Offline:** save the page (File → Save Page As). It is a single self-contained HTML file.

## Navigation

- **Zoom:** mouse wheel (around the cursor), the − / + buttons, or <kbd>-</kbd> / <kbd>+</kbd>.
- **Pan:** drag with the left mouse button.
- **Fit:** double-click, the Fit button or <kbd>0</kbd>.
- **1:1:** one drawing pixel per screen pixel (raster only), <kbd>1</kbd>.
- **Overview:** the minimap in the corner; click or drag in it to move the view.
- **Pages:** ‹ / ›, <kbd>←</kbd> / <kbd>→</kbd> or <kbd>Page Up</kbd> / <kbd>Page Down</kbd> step
  through multi-page TIFFs, HP-GL plots, DWF sheets and CGM pictures.

## Toolbar

Some buttons appear only for the formats they apply to.

| Button | Key | What it does |
| --- | --- | --- |
| Paper | <kbd>I</kbd> | Cycles the background: dark blue, white, black. Also fixes raster files written with the wrong polarity. |
| Grid | <kbd>G</kbd> | AutoCAD-style grid with the world axes (vector files). |
| Line weights | <kbd>W</kbd> | Draws DXF/DWG line weights above 0.25 mm at their size on screen, as AutoCAD's LWDISPLAY does. Off by default. |
| Layers | <kbd>L</kbd> | Show or hide DXF/DWG/DWF/WebCGM layers or HP-GL pens, with a name filter and All on / All off. Alt-click shows only that layer. Layers that are off or frozen in the file start hidden. |
| Find | <kbd>Ctrl</kbd>/<kbd>⌘</kbd>+<kbd>F</kbd> | Finds text in vector drawings; <kbd>Enter</kbd> / <kbd>Shift</kbd>+<kbd>Enter</kbd> step through matches. |
| Measure | <kbd>M</kbd> | Opens the measure bar (below). |
| Save PNG | <kbd>Ctrl</kbd>/<kbd>⌘</kbd>+<kbd>S</kbd> | Saves the current view as a PNG. |
| Info | | File properties (see below). |
| Shortcuts | <kbd>?</kbd> | Lists every keyboard shortcut. |

## Measuring

- **Distance** (<kbd>M</kbd>), **Angle** (<kbd>A</kbd>: arm, vertex, arm) and **Area** (<kbd>R</kbd>).
- Area: click the corners, then close on the first corner, double-click or press <kbd>Enter</kbd>.
- <kbd>Backspace</kbd> removes the last point; <kbd>Esc</kbd> stops measuring.
- In vector drawings points snap to line ends, midpoints, centres and the axes. Hold
  <kbd>Shift</kbd> to place a point freely.
- **Keep** leaves finished measurements on the drawing; **Clear** removes them.

## File information

| Format | Shown |
| --- | --- |
| TIFF | Size, resolution, compression and tags |
| CALS | Header records |
| DXF/DWG | Version, units, layers and entities not drawn |
| HP-GL | Language, plot size, pens and raster images |
| DWF | Version, sheet, paper size and metadata |
| CGM | Version, description, fonts, picture and scale |
