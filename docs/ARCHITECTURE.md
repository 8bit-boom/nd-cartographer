# Architecture (M0 + M1a start)

The full design is [ND_CARTOGRAPHER_PLAN.md](ND_CARTOGRAPHER_PLAN.md); this file
tracks what actually exists, crate by crate, as milestones land.

## Crates today

- **`ndcart-core`** — pure document model, no IO:
  - `grid::RoomGrid` — the sparse room grid (`BTreeMap` of painted cells →
    spaces; marks on edges). Domain vocabulary matches nd-world's room-tool
    state so publishing is a pass-through (plan §5.1).
  - `walls::walls_from_grid` — the research-proven rule: *an edge between two
    different spaces is a wall*; boundary edges (space vs. nothing) emit too;
    an interior wall derives exactly once (West/North emit on difference,
    East/South only on empty). Marks override kinds (`Opening` removes the
    segment); collinear runs of the same kind+state merge. Output is
    nd-world's wall JSON shape, in map pixels.
  - `journal` — append-only command journal; every op stores the inverse
    computed against the state it was applied to (undo is exact, replay is a
    fold, NdJSON on disk). Refused ops are never journaled.
  - `doc` — manifest, floors, lights (≤ 60, the nd-world cap), labels.
- **`ndcart-server`** — the `ndcart` binary: axum app serving the M0 UI,
  the doc/ops/undo API, tiles (tiny-skia bake on a zoom pyramid, 16 px/cell
  at z0), optional `--data-dir` persistence, and `bench`.

- **`ndcart-formats`** — Universal VTT read/write (`uvtt`): walls/secrets →
  `line_of_sight`, doors → `portals`, lights as `aarrggbb`; windows ride in an
  `x_ndcart.windows` extension (exact ndcart round trip, ignored by other tools).
  Import validates every number and caps sizes. Served by
  `GET /api/docs/{id}/export/uvtt[?floor=]` (no picture yet — needs the full-map
  render, which nd-world's `import-uvtt` requires).
- **Server hardening** — off-loopback listening requires a token (`ND_CARTO_TOKEN`,
  `ND_CARTO_TOKEN_FILE`, or `<data-dir>/token`; Bearer, cookie or `?token=`;
  `/health` public). Saves are temp-file + fsync + rename.

- **Full-map render + export** (`tiles::bake_full_as`): PNG, WebP (lossless,
  pure-Rust encoder — a lossy WebP for the plan's "q≈85 cap fitting" needs libwebp
  and is still to do) and AVIF (lossy, ravif). The tile baker and the exporter
  share one renderer. Fixed along the way: wall strokes were drawn at twice their
  size because lattice pixels were scaled by pixels-per-cell.
- Light range is capped at 60 squares (nd-world's cap), not 200.

- **`ndcart-bridge`** — the optional nd-world client (cargo feature `bridge`,
  default on; CI also builds `--no-default-features` for standalone parity).
  `login` (`POST /api/login`, 2FA / lockout / non-GM mapped to plain errors),
  `probe` (`/health` → build + mode; `import_uvtt` is false on the Rust-only
  image), and `publish`: create → picture → grid → walls → lights → rooms through
  the *per-part* routes that both images serve, in place on re-publish, every
  cap (5,000 walls, 60 lights, 100..20,000 px canvas, picture type) checked
  before the first request, and a failure after creation reports the slug.
  `rooms` translates our grid into nd-world's room-tool state (small integer
  space ids, run-length cells, marks as top/left edges).
- **`ndcart publish DOC --server URL --email E [--world W] [--floor ID] [--ppc 32]
  [--format webp|avif|png] [--data-dir DIR]`** — password only from
  `ND_CARTO_PASSWORD` / `ND_CARTO_PASSWORD_FILE` (never an argument), 2FA from
  `ND_CARTO_TOTP`. The slug is stored in the manifest (`nd_world`), so the next
  publish updates the same map; a half-finished publish is resumed, not duplicated.
- **`ndcart convert IN OUT [--format] [--quality]`** and `POST /api/convert` —
  PNG/JPEG/GIF/BMP/WebP in; PNG/WebP/AVIF out (AVIF *input* needs dav1d, a C
  library, so it is refused with a clear message).

Verified end to end against a real nd-world in Python mode *and* the Rust-only
gateway (built from the current tree, no Python behind it), on throwaway data
directories: publish, re-publish with an AVIF background, walls/lights/rooms read
back correctly. That run found and fixed two bugs unit tests had missed — saving
a document with a door panicked (edge marks were a struct-keyed map, which JSON
cannot express), and `publish` started a nested tokio runtime.

- **Linter** (`ndcart-core::lint`, `GET /api/docs/{id}/lint`) — unreachable rooms
  (a window does not connect; a door, secret door or opening does), doors/windows
  on nothing / inside a room / into the void, lights outside every room, unused
  spaces, and nd-world's caps (errors that block publishing). Fixes are ordinary
  undoable ops. `ndcart publish` lints first and refuses on errors.
- **UI** — door and window tools (click a cell edge), a Problems panel with Go-to
  and Fix, export links (PNG / WebP / AVIF / UVTT), F1 help generated from the same
  table that handles the keys. Doors, windows and secret doors draw in their own
  colours.
- **Cap fitting** — `ndcart publish --max-mb N` (default 20) steps the picture's
  resolution down by a quarter at a time (never below 8 px per square), re-deriving
  lattice, walls, lights and rooms at each scale, until it fits under the server's
  upload cap.

- **One secrets filter** (`ndcart-core::view::for_audience`) — every player-facing
  output goes through it: a secret door becomes a plain wall (its mark is dropped,
  so the edge derives as an ordinary wall) and GM-only labels (`gm_only`) are
  removed. Used by `?audience=player` on the PNG/WebP/AVIF and UVTT exports, by
  `ndcart publish --audience player` (picture, walls, lights *and* the room state,
  which carries marks), and by the UI's "players' copy" checkbox. Tests assert no
  `secret` survives in the player state, the exported picture, or the publish
  payload. The upcoming true-preview must use the same function.

- **Outliner v1** (`ndcart-core::outline`, in every floor view as `outline`) —
  rooms with square counts and extents (sorted by name, case-insensitive), lights
  and notes with the cell they sit in. The UI renders it as an ARIA tree (roving
  tabindex; ↑↓ move, ←→ fold, Enter goes to the item, F2 renames a room, Delete
  removes a light or note; letters inside the tree never fire the tool keys) —
  the keyboard-complete path the plan calls "the accessible map". Not yet:
  zones, drag-and-drop, multi-select bulk ops (M3).

- **`ndcart-paint`** (M1b begins) — materials, not colours. A material is a
  procedural pattern (tiles, planks, cobble, mottled ground, water) sampled in
  *world cell coordinates*, so it looks the same at every zoom, in every tile and
  in a full-map export: no seams, nothing to ship. Deterministic from
  `(material, u, v, seed)` using only `+ - * /`, `floor` and integer hashing (no
  `sin`/`powf`), so output does not drift between platforms. Adjacent materials
  **blend automatically**: the boundary jitter enters with opposite signs on the
  two sides of an edge, so the join is continuous by construction (the first
  version added jitter to the weight and left a seam — caught by test). The
  **first-party starter pack** is ten procedural materials (stone floor,
  flagstone, wood planks, cobblestone, grass, dirt, sand, water, metal deck, neon
  tile): MIT, nothing to license or redistribute.
  A space carries an optional `material` (validated: 1–40 chars of `a-z0-9_`;
  old documents stay flat; an unknown id falls back to the flat `color`).
  `GET /api/materials` feeds the UI palette. Tests: a tile equals the matching
  crop of the full render; the blend is continuous across an edge; materials are
  textured, distinguishable and stay near their base colour.
  **Budget (plan §6):** the full-map bake at 32 px/cell, every cell bordering a
  different material (the worst case), runs at ~2.4 s per 10 Mpx in release — a
  full 200x140 map is about 7 s against the 30 s budget. `ndcart bench [--ppc N]`
  measures it and asserts the budget. Textured pictures are big as lossless
  PNG/WebP (17 MB for that slice), so the cap-fitting step and, still to do,
  lossy WebP matter more now.
- **Brush engine** (plan §5.3, §7.1) — painting is *data, not pixels*. A `Stroke`
  (`ndcart-core::stroke`) is one journal op: brush, material or colour, pointer
  samples with pressure and tilt, size, opacity, hardness, roughness, density and
  a seed. Strokes sit in z-order on the floor (`strokes`), are validated at the
  boundary (finite, bounded, ≤ 4,000 points, ≤ 20,000 strokes, ids unique), are
  cosmetic (walls, rooms and what nd-world stores are unchanged; only the baked
  picture differs), and `add_stroke` / `remove_stroke` have exact inverses — an
  undone removal returns to its original z-index. `ndcart-paint::Prepared` turns
  a stroke into deterministic coverage at any world position: **Round** (width
  from pressure and tilt, soft↔hard edge, hand-drawn roughness) and **Scatter**
  (seeded pebbles/tufts laid along the path, density-controlled). A spatial index
  sized to the brush means a pixel only looks at nearby segments or dots. The
  baker composites strokes over the cell layer in z-order; the **eraser reveals
  the room layer beneath** (mixes back toward the pre-stroke pixels), so
  removing or erasing never leaves holes. Tests: a tile equals the matching crop
  of the full render with strokes crossing tile boundaries; eraser, z-order and
  removal restore the exact picture; hostile numbers are refused; scatter is
  capped and seeded. Heavy case (300 small + 40 long rough strokes of 500
  points, 40x40 cells at 16 px): ~300 ms debug, ~60 ms release.
  UI: Brush (R) and Eraser (E) tools with size / opacity / hardness / rough edge
  / density controls and a Round/Scatter choice, `[` `]` for size; the pen's
  pressure and tilt are recorded (a mouse counts as full pressure), the stroke
  previews on the canvas at once and is sent as one op when you lift. Picking a
  material swatch keeps the current paint-like tool.
  *Not yet:* pattern-stamp brushes, stamps/props with drop shadows, fill, line and
  rectangle brush variants, a stroke list in the outliner, and caching prepared
  strokes across tiles.

- **Stamps** (props) — `ndcart-core::stamp` + `ndcart-paint::stamps`. A stamp is a
  placement (asset id, centre in cells, scale 0.1–8, rotation, flip, tint, shadow)
  with ops `add_stamp` (z-index kept on undo), `update_stamp` (move/rotate/re-tint
  in place, exact inverse) and `remove_stamp`. The first-party pack is twelve
  props defined as vector primitives in a unit box (crate, barrel, table, chair,
  pillar, torch, bed, rug, chest, tree, bush, rock): MIT, tiny, resolution
  independent. The baker draws each stamp into its **own sprite** in
  tile-independent coordinates and blits it at an integer offset, so a tile equals
  the same crop of the full render **exactly** (a path's anti-aliasing otherwise
  depends on its offset — caught by test at 11/255). Soft shadows are three
  low-alpha layers thrown down-right in proportion to the prop's height (rugs and
  torches lie flat). `GET /api/stamps` and `GET /api/stamps/{id}/icon.png` feed the
  UI's Stamp tool (`S`; `,` `.` rotate, `F` flips; snap to ½ cell) and the
  outliner's Props group.
- **Journal undo is now linear history.** Undo used to *swap* the undone entry for
  its inverse, which lost the forward op (replaying the journal then failed on any
  document that had undone an add/remove) and made a second Undo toggle instead of
  step back. Undo now appends an entry marked `undoes: <index>`; forward entries
  are never rewritten, repeated undo walks back, and replay from the start always
  reproduces the document. Old journals still load.
- **Wall ids are stable** — derived from the run's axis, line, start and kind, so
  a re-publish keeps the same id for a door that has not moved and nd-world keeps
  its runtime open/closed state.

- **Underlay, imported walls, assets, and opening a `.dd2vtt`.** A floor can hold a
  raster **underlay** (an asset id — the picture's SHA-256 — plus its rectangle in
  cells and an opacity), drawn over the void and under the rooms, and **imported
  walls** (cell coordinates, not derived from rooms: an opened Dungeondraft map has
  walls but no rooms). `floor_walls` is the one place derived and imported walls
  meet, so exports, publishing and the linter agree; the secrets filter turns an
  imported secret door into a plain wall for players. The **asset store**
  (`GET/POST /api/assets`) is content-addressed, refuses anything that does not
  decode as an image within the size limits, writes atomically under
  `<data-dir>/assets/`, and caches decoded pixmaps. `POST /api/import/uvtt` turns
  a Universal VTT file into a project in one undoable step (picture → underlay,
  line-of-sight/portals/windows → imported walls, lights → lights, the file's
  squares → the grid) and leaves nothing behind if any part fails. The UI has a
  project picker, "New" and "Open .dd2vtt…". A round trip (export UVTT → open →
  export) preserves walls, the door portal, the window, the light and the size
  (test). Export extent now covers *every* layer (`FloorDoc::extent`): before, a
  stroke or stamp outside the painted rooms was cropped from exports.
  Underlays are scaled bilinearly; very large pictures at low zoom will alias until
  mip-mapping lands.
- The linter treats a picture-only map as non-empty and does not call a light on
  the underlay "in the void".

- **Terrain (M2 begins)** — `ndcart-core::terrain` + `ndcart-terrain`. A floor can
  hold a sparse heightmap: 16-bit elevations in 64×64-sample chunks, 4 samples per
  cell, absent chunks reading as open ocean. Edits are deterministic ops with
  **exact undo by chunk snapshot** (an undone first edit removes the terrain
  again; a removed-then-restored river does not carve a second time):
  `terrain_stroke` (raise / lower / smooth / flatten / roughen — points, radius,
  strength, seed), `terrain_set` (raw chunks: generators and undo),
  `set_terrain_style` (sea level, tint, saturation, contrast, brightness, relief
  strength, contour interval), `add_river` / `remove_river` (a river is drawn
  upstream→downstream; the carved bed never rises and banks blend smoothly),
  `clear_terrain`, `set_canvas` (the map's size in cells — authoritative for the
  export extent). Chunk data travels as base64 so a 160×110-cell map is ~0.7 MB
  of JSON, not 70 chunks × 4,096 numbers.
  **Shading** (`ndcart-terrain::Shader`) is a pure function of position: tints
  that follow altitude with a moisture field (dry lowlands tan, wet ones green),
  bare rock on steep ground, snow on the heights, depth-shaded water with a pale
  shore, relief from a north-west sun, optional contours, then the global grade.
  Terrain and rivers (rivers are variable-width soft strokes — the brush engine
  again) bake **pixel-identically** in tiles and in the full render (test).
  **Generation** (`ndcart-terrain::generate`) is seeded and platform-stable (no
  `sin`/`powf`): domain-warped fractal continents, ridged mountain chains, a sea
  level chosen so the requested land fraction is met, a height curve that keeps
  most land low, and rivers traced downhill from high ground. `POST
  /api/docs/{id}/generate` replaces a floor's terrain and rivers as **one undoable
  step** (same seed ⇒ byte-identical picture).
  *Not yet:* biome brushes, mass scatter, typography, routes, regions, hex
  overlay, publish B, and the terrain tools in the UI.

- **Overlay items and publish B.** Markers (points of interest, optionally linking to an
  nd-world battle map), regions (tinted, bordered, optionally hatched polygons) and
  routes (dashed or solid with waymarks) are one op family — `put_overlay` /
  `remove_overlay` — with exact undo (a removed item returns to its z-index) and the
  same boundary validation as everything else (ids, finite bounded coordinates, colours,
  per-list caps matching nd-world's 500). The baker draws them over everything else;
  paths are built in world pixels and shifted by an exact integer, so tiles equal the
  full render **exactly** (test). `ndcart publish` branches on the project kind: a
  **world map** is baked whole and sent as an nd-world image map — created with
  `POST /maps/new`, re-published in place through `/maps/{slug}/upload`, markers and
  regions through the overlay replace route. Verified end to end against a real
  nd-world in Python mode and the Rust-only gateway (markers at exactly the right
  `lat`/`lng`, region intact, AVIF picture swapped, no duplicates on re-publish).

- **Typography** (plan §7.2). The bundled font is Crimson Pro (SIL OFL 1.1; the licence
  text ships in `crates/ndcart-server/assets/fonts/`), read with `ab_glyph`. A **label**
  is text in document *cells* (position and font size scale with the map): straight
  (centred, turned by `rotation`) or **along a path** — the line is read left to right
  whichever way the path was drawn, follows bends glyph by glyph, and shrinks to fit a
  short path (or is declined if it would have to shrink too far). Options: bold, colour,
  tracking (letter-spacing), a halo that flips light/dark with the ink so text reads over
  any ground, and `gm_only` (stripped by the same player filter as everything else —
  tested end to end through the export route). Kerning is applied; a character the font
  lacks leaves a gap instead of the missing-glyph box. Names are generated, too: a
  marker's name beside its pin, a region's name at its centroid (letter-spaced, never wider
  than the region), river and route names along their lines. A GM's own labels win every
  collision; a generated name that collides is retried at 80% and 65% before it is
  dropped; text under 5 px is not drawn (so a zoomed-out map is not covered in
  illegible smudges). Glyph outlines are cached vector paths; each label is drawn into
  its own sprite at an integer offset, so tiles equal the full render **exactly** (test).
  UI: Label (`T`) and Text-on-path (`Y`) tools with size, colour, bold, halo, spacing, turn
  and GM-only controls; F2 renames and Delete removes from the outliner's Notes group.
  Fix found on the way: markers, regions, routes and labels now grow the export extent
  (they were cropped from exports of maps without an explicit canvas).

## Floors

All floors share one journal; each entry carries `floor: Option<String>` and undo/replay only touch entries for their own floor
(legacy entries without a floor match any floor). Floor structure (add / remove / rename / move) lives in a separate `StructJournal`
(`structure.ndjson`) with its own undo; removing a floor snapshots it so undo restores it. `apply_ops` is atomic. The tile route can
composite another floor underneath (`onion`, 30%). The UI floor bar offers add, duplicate, rename, reorder, remove, undo and onion skin.
`ndcart publish --all-floors` publishes each floor as its own nd-world map ("Doc – Floor"); slugs are kept per floor in `NdWorldLink.floor_slugs` so re-publishing updates in place, and the legacy `slug` is the first floor's. Not yet: stairs links.

## Still open in M0

WebGL viewport, WebSocket change feed, `bench` budget assertions in CI, the
100k-object / kill -9 acceptance tests.

## Not yet (per plan)

`ndcart-paint` (brush engine), `ndcart-terrain` (heightmaps),
`ndcart-formats` (UVTT/Foundry/packs), `ndcart-render` (the real baker + LOD
golden tests), `ndcart-ai` (MapSpec + proposals), `ndcart-bridge` (the
optional nd-world client), `ndcart-desktop` (Tauri shell). The WebGL viewport
replaces the M0 Canvas2D renderer when tiles get LOD-heavy; the tile contract
(`/tiles/...`) is designed so the swap is client-side only.

## Invariants worth keeping

1. **Every number crossing a boundary is validated** (finite, typed,
   bounded) — journal ops, imports, published JSON (plan §17.7).
2. **The journal is append-only; nothing is deleted.** Undo applies stored
   inverses as new entries.
3. **Core stays pure.** No IO, no clock reads at call time (ids are the one
   pragmatic exception), no axum in `ndcart-core`.
4. **Domain vocabulary = nd-world's.** Publishing is a translation, not a
   re-derivation.
5. **Walls derive, never hand-draw** — the edge rule is the single source.
