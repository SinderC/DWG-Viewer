# Supported formats

Every format is decoded locally in the browser. "Not drawn" lists what is read but not yet shown.

## CALS raster

Type 1.

## TIFF

- Black/white: CCITT G4, PackBits, uncompressed.
- Grey/colour: LZW, Deflate, JPEG, PackBits, uncompressed.
- Multi-page files.
- Colour images are limited to 200 megapixels.

## DXF

ASCII DXF, model space.

- Lines, rays and construction lines; polylines with arcs and widths; circles, arcs, ellipses,
  splines, helices, points.
- Filled SOLID/TRACE, 3DFACE outlines, hatches (solid, patterned and gradient), wipeouts.
- TEXT/MTEXT (inline formatting, stacked fractions, background masks) and attributes.
- Leaders and multileaders, multilines, tables, tolerances.
- Nested blocks; dimensions (drawn from their points when the block is missing).
- Polyface/polygon meshes and MESH; 3DSOLID/BODY/REGION as a top-view wireframe.
- Layer and entity colours, transparency, linetypes and line weights (line weights are shown with
  the line-weight toggle).

Not drawn: images, binary DXF.

## DWG

R13 to 2018+, read with the [`acadrust`](https://crates.io/crates/acadrust) crate. The same
model-space entities as DXF. R12 and older are not supported.

## HP-GL, HP-GL/2 and HP RTL

Plot files (`.plt`, `.hpgl`, …), plain or wrapped in PCL/PJL.

- Lines, encoded polylines, arcs, circles, rectangles, wedges, Béziers.
- Polygon fills (solid or shaded), labels, pen colours and widths.
- Multiple pages (`PG`).
- RTL raster images (1-bit, 8-bit indexed or 24-bit, compression 0–3), placed from the page's
  top-left corner.

Not drawn: line types, hatch patterns (filled solid), clip windows, planar RTL colour. Fills are
drawn under all lines.

## DWF and DWFx

Classic single-stream DWF (up to 5.5, including zlib-compressed sections) and DWF 6 packages (one
page per ePlot sheet):

- Lines, polylines, polygons, triangle strips, contour sets, circles, arcs, ellipses, text.
- Colours and colour maps, line weights, layers.
- Embedded images: indexed, mapped, RGB/RGBA, JPEG, PNG, Group 4.
- Paper scale from the sheet descriptor or `PlotInfo`.

Not drawn: bitonal and Group 3X images, raster overlays, line patterns, markers, 3D (W3D) sections.
Gouraud shading is drawn flat.

**DWFx** (XPS pages): paths with solid fills and strokes, curves and arcs, text (in the viewer's
font), PNG/JPEG/TIFF image fills, transforms and resource dictionaries.

Not drawn: gradients (drawn in their first colour), visual brushes, clipping, opacity.

DWF, DWF 6 and DWFx are tested only on synthetic files.

## CGM

Binary and clear-text encodings, including WebCGM. One page per picture.

- Polylines, polygons and polygon sets, rectangles, circles, ellipses and their arcs (open, pie and
  chord), Béziers, markers.
- Text: all four text paths; restricted text shrunk to its box; fonts as sans-serif, serif or
  monospace, italic and bold by name.
- Colour tables and direct colour (RGB, CMYK).
- Line and edge widths and types, solid and hatch fills.
- Cell arrays (rotated or skewed too), Group 4/JPEG/PNG tiles, WebCGM layers.

Not drawn: pattern fills (drawn solid), NURBS and conics, clipping, character spacing on horizontal
text.

The standard leaves nominal marker size, hatch spacing and dash lengths to the device; the viewer
picks its own. Clear-text tiles and layers are tested only on synthetic input.
