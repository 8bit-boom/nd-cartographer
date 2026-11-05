# nd-cartographer — a standalone TTRPG map creator & editor, with deep nd-world integration

**Status (2026-11-05, revised): M0 and M1 are complete, M2 is mostly built — see
[ARCHITECTURE.md](ARCHITECTURE.md) for what exists and §20 below for the done/remaining
table. This document is the design; §19 records the review pass that revised the milestone
order and corrected nd-world facts.**

Plan for a Rust desktop + Docker application that is a **full map creator in its own
right** — battle-map painting in the Dungeondraft class, world/region map-making in the
Canvas of Kings class, big-and-complex made easy, AI as a co-cartographer — that also
publishes into a running nd-world instance over its verified HTTP API and interchanges
with the wider VTT ecosystem (Universal VTT, Foundry, art packs). It is fully usable
with no nd-world at all; nd-world makes it better.

Throughout, "the studio" means nd-cartographer (earlier drafts said "companion" —
same app, bigger ambition).

Everything marked **[verified]** below was read out of this repository (file:line given)
or measured by [MAP_CREATOR_RESEARCH.md](MAP_CREATOR_RESEARCH.md). Anything marked
**[verify]** must be re-checked against the live server at implementation time.

---

## 1. One-liner and positioning

> **nd-cartographer is a complete map-making application that stands on its own.**
> Paint battle maps like Dungeondraft, draw worlds like Canvas of Kings, generate and
> AI-draft at any scale — and if you run nd-world, one press makes the map *playable*
> there (fog, tokens, combat link, TV mode) seconds later.

Three positions at once:

1. **A standalone product first.** Every feature works with nd-world absent, offline,
   for any TTRPG system (nd-world itself hosts four rule systems — N&D, Hunt, Worm,
   Asterion — and other tables use Foundry, Roll20, or paper). No feature may require
   the server; the bridge (§9) is one optional, dormant-until-configured module.
   Exports cover the whole ecosystem: UVTT, Foundry scenes, image maps, PDF.
2. **A different class of tool from the in-app editor — not a rewrite of it.**
   nd-world's schematic editor is a browser SVG page, smooth to ≈ 1,300 elements,
   ceiling ≈ 6,000 objects **[measured, research §6]** — right for *editing at the
   table*. A painting and world-building studio (materials, heightmaps, thousands of
   stamps and labels) needs a tiled, LOD, native-speed engine — exactly what the
   research said *not* to build inside nd-world ("WebGL only if measurements ask for
   it"), and exactly what a standalone native app is free to build. The repo sanctions
   the split: `CLAUDE.md:27` — *"The Android app and desktop editor live in the
   sibling repository, not here."*
3. **Integration as a flagship feature, not an afterthought:** play-ready publishing
   (§9), world-grounded AI through the server's queue (§8), the table remote — the
   deepest nd-world link of any map tool, and still just one module.

**What it is not:** not a VTT runtime (no tokens-in-play — nd-world/Foundry own play);
not a photo editor (painting is constructive — materials, stamps, heightmaps — not
curves/masks/photo filters); not a collaborative whiteboard (single editor, crash-safe
journal).

---

## 2. Goals and non-goals

**Goals**

1. **A complete standalone creator.** Empty canvas → finished map with zero
   dependencies: paint, stamp, scatter, generate, label, style, export — for any TTRPG
   system, any table, offline.
2. **Dungeondraft-class battle maps.** Material painting with automatic edge blending,
   pattern brushes, pressure-sensitive input, stamps with shadows and recoloring,
   painted light, caves; print-quality export.
3. **Canvas-of-Kings-class world maps.** Heightmap terrain (hillshade, hypso, biomes),
   rivers that carve, mass scatter as density fields, real typography (text along
   paths), political regions, journeys, hand-drawn style packs.
4. **Big made easy.** 200×140-cell dungeons, multi-floor stations, city districts,
   regional maps with hundreds of POIs — fluid on a mid-range laptop (the GM's
   Vivobook-class machine the cockpit docs mention), with an outliner, bulk tools, and
   generators doing the repetition.
5. **AI that actually helps.** AI proposes; the GM disposes — every AI output is a
   reviewable proposal batch, never a silent edit (nd-world's own media-rename /
   template-draft pattern). AI works through nd-world's model config and its one-GPU
   queue when connected, and against a direct OpenAI-compatible/Ollama endpoint when
   standalone. And it creates the art itself — textures, object stamps, and
   mini-packs (§8.5).
6. **Play-ready nd-world integration.** One press produces a *playable* nd-world map —
   background, grid, walls, doors, lights, room state, labels — and re-publish updates
   it in place. The deepest integration of any map tool; entirely optional.
7. **Ecosystem interchange.** Universal VTT in and out, Foundry scenes out (in by M5),
   Dungeondraft/Wonderdraft art packs in, PNG/WebP/PDF out.
8. **Windows and Linux, desktop and Docker.** One codebase: Tauri v2 desktop shells +
   the identical server binary in a container for browser use.

**Non-goals (v1)**

- Real-time multi-user editing; per-player secret logic (nd-world's strict secrecy owns
  that); campaign management; animation/video; 3D.
- General raster/photo editing (curves, masks, photo filters) — painting here is
  constructive: materials, stamps, heightmaps.
- Atlas/booklet layout design (tiled page *export* is in; book layout is not).
- Bundled community art packs (license trap — research §6) and a community pack hub
  (question 7); i18n in 1.x.

---

## 3. The three map kinds

| Kind | Feels like | Grid model | Publishes as |
|---|---|---|---|
| **Battle map** | Dungeondraft-class painting + generators | square or hex cell lattice; rooms/corridors as *spaces*; walls derive from edges between different spaces **[tested, research §3]**; materials/stamps/patterns on top | nd-world **schematic** (playable: walls, doors, lights, fog-ready) + UVTT + Foundry + PNG |
| **Region/world map** | Canvas of Kings-class studio | heightmap terrain (painted, generated, or imported) + vector layers over raster underlays; zones nest; rivers/roads as styled splines | nd-world **image map** (`/maps/new` + Leaflet tiles) with markers/regions overlays |
| **Schematic diagram** | the in-app SVG editor, big | free canvas, nd-world's exact element schema | nd-world **schematic** via importer `schematic_elements` (merge-by-id, verified) |

One project may hold several floors of a battle map, or a region map with embedded
battle-map links (an overlay marker's `link_schematic` field **[verified]**,
`app/templates/map_viewer.html:540` — a POI can jump straight into its battle map).

---

## 4. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│  ui/  (TypeScript, WebGL2/Canvas2D viewport, vendored deps)      │
│  tiles in · live GPU brush-stroke compositing · edits out —      │
│  no business math lives here                                     │
└─────────────▲──────────────────────────────────▲─────────────────┘
              │ HTTP + WebSocket (doc ops, tiles, AI progress)
┌─────────────┴──────────────────────────────────┴─────────────────┐
│  ndcart (axum 0.8 server binary — same binary everywhere)        │
│    core        doc model, journal, structure, generators          │
│    paint       brush engine, materials + blending, patterns      │
│    terrain     heightmap tiles, hillshade, rivers, biomes        │
│    render      tile baker (tiny-skia + rayon), LOD pyramid       │
│    ai          MapSpec, proposals, LLM/imagegen clients          │
│    bridge      nd-world client: login, publish, table remote     │
│    interchange UVTT · Foundry scenes · art-pack formats          │
└─────────────┬──────────────────────────────────┬─────────────────┘
   Tauri v2 shell (window, .ndmap          Docker: ndcart serve,
   association, installers) → the          browser UI, /data volume
   same server on 127.0.0.1:random
```

**One server binary, three ways to run it** (mirrors nd-world's gateway/entrypoint
pattern):

- `ndcart` / `ndcart-desktop` — desktop: server binds loopback, Tauri window opens on
  it; browser fallback via `ndcart serve --open`.
- `ndcart serve` — Docker/browser: identical UI over the network.
- `ndcart <cli>` — headless: `gen`, `export`, `publish`, `bench` (scriptable, CI-able,
  and the door to an MCP surface later).

### Crate layout (new sibling repo `nd-cartographer`)

| Crate | Contents |
|---|---|
| `crates/ndcart-core` | document model, command journal, room-grid/walls-from-rooms engine, generators, MapSpec schema (schemars), export presets. Pure Rust, zero IO — the most-tested crate. |
| `crates/ndcart-paint` | the painting engine: brush stroke model (pointer samples with pressure/tilt), material system with automatic edge blending, pattern/scatter brushes, painted lights — deterministic from op + seed. Pure Rust. |
| `crates/ndcart-terrain` | heightmap terrain: tiled 16-bit elevation, hillshading/hypso rendering, water depth, river carving, biome mapping, global tint/grading. Pure Rust. |
| `crates/ndcart-formats` | interchange: UVTT read/write, Foundry scene read/write, `.dungeondraft_pack` / Wonderdraft-folder import, `.ndpack` read/write. |
| `crates/ndcart-render` | chunk → tile baker (tiny-skia + image + rayon), LOD pyramid, asset rasterization (PNG/WebP/AVIF decode; resvg for SVG stamps). |
| `crates/ndcart-server` | axum app: static UI, REST doc ops, WebSocket change feed, tile routes, health; auth gate for non-loopback mode. |
| `crates/ndcart-bridge` | nd-world HTTP client (reqwest + cookie store): `/api/login`, world targeting, publish/update flows, capability probe. |
| `crates/ndcart-ai` | LLM/imagegen clients (via bridge, or direct OpenAI-compatible/Ollama), proposal batching, validation + retry. |
| `crates/ndcart-desktop` | Tauri v2 shell (feature-gated; excluded from the Docker build). |
| `ui/` | TypeScript + Vite; **all dependencies vendored** with versioned filenames and license files, exactly like `static/vendor/` **[verified convention]**. |

Conventions adopted from `rust/` **[verified]**: edition 2021, axum 0.8 / tokio 1 /
serde_json (`preserve_order`) style dependency set, release profile `lto = "thin"`,
`codegen-units = 4`, `strip = true`, `--locked` everywhere, mimalloc optional.

### Why this shape (the one big decision)

The requirement set — *Windows desktop app AND a Linux Docker image, in Rust* — rules
out a purely native GUI (egui/iced cannot be served from a container to a browser in
any honest way). The split that satisfies all three deliverables is a **Rust-first
hybrid**: every piece that matters for correctness and performance (document,
geometry, the painting and terrain engines, baking, generators, AI validation,
publishing, interchange) is Rust; the UI layer is a thin, typed canvas client.
Painting raises the bar for the UI, and browsers have already cleared it — Figma and
Photopea are painting-class applications in a tab — so the input loop is explicit
(§6): strokes composite on the GPU in the viewport for zero-perceived-latency
feedback while Rust bakes the authoritative pixels asynchronously. A second benefit:
the team's existing canvas/SVG editor expertise (the in-app schematic editor,
`map-rooms.js`, `map-vision.js`) transfers directly. Alternatives considered are in
§13 (decision log).

---

## 5. The document model (`.ndmap`)

A project is a **folder** (zippable/shareable), memory-mappable, git-diffable in
spirit:

```
my-dungeon.ndmap/
  manifest.toml        # id, name, kind, created, nd-world link (url, world slug,
                       #   published slugs + content hashes for re-publish), export presets
  floors/<id>/doc.json # structure tree + geometry for one floor (snapshot)
  ops/000123.ndjson    # command journal since snapshot (crash-safe, replayable)
  assets/              # content-addressed by sha256: stamps, textures, image-gen underlays
  cache/tiles/...      # baked LOD tiles — regenerable, deletable
```

### 5.1 Battle-map geometry — aligned with nd-world by design

The core adopts nd-world's proven room-grid model verbatim (same field names), so
publishing is a pass-through, not a translation:

- **Room grid state** = nd-world's `rooms.json` contract `{cell, ox, oy, cols, rows,
  cells, spaces, marks}` **[verified, `app/routers/map_walls.py:146-159`]** — internally
  stored sparse/chunked (RLE per 32×32 chunk) so a 512×512-cell city map is cheap;
  flattened to nd-world's form on publish.
- **Walls** = `{id?, pts: [[x,y],...], kind: wall|door|window|secret, state:
  open|closed}` **[verified, `map_walls.py:64-78`]**, ≤ 5,000 per nd-world map. Derived,
  not hand-drawn: an edge between two different spaces is a wall; doors/windows/secrets
  are marks on edges **[tested, research §3]**. Merged into long segments (the
  measured 100×70 dungeon is 749 segments; 200×140 is 2,878 — both far under the cap).
- **Lights** = `{id?, x, y, range, color, intensity, on, label}` ≤ 60 **[verified,
  `map_walls.py:181-195`]**.
- **Floors/props/labels** render into `elements_json` shapes (`rect`, `text`, `image`,
  … full list in [AI_SCHEMATIC_GUIDE.md](AI_SCHEMATIC_GUIDE.md); the importer's accepted
  set is `rect, circle, line, arrow, poly, path, text, pin, image, measure, token`
  **[verified, `app/routers/importer.py:46`]**).
- Multi-floor: one grid per floor; stairs/lifts carry `links_to` floor+cell so AI and
  generators can reason vertically.

### 5.2 Region-map geometry

Zones (nested labeled polygons), polylines (roads/rivers with width + style), POIs
(markers), raster underlays (0..n, blend mode + opacity), label layers with collision
avoidance. Publishes to markers/regions: `{lat, lng, label, note, color,
link_schematic}` and `{points, name, color, opacity}` **[verified]**.

### 5.3 Paint and terrain data (the Dungeondraft/CoK half)

Painting is **data, not pixels**: a stroke is a journal command `{tool, material /
stamp / brush, points[] (x, y, pressure, tilt), size, opacity, seed}`, and a paint
layer is its op list — re-baked deterministically into chunk tiles under the same
replay discipline as geometry, so undo, AI diffs, and reproducible exports stay
uniform. Overland terrain is a tiled 16-bit heightmap (elevation and water are painted
like any material) plus slope/altitude-derived material assignment, so "raise this
range" is one op, not ten thousand pixels. Image underlays (imported art, AI
generations) remain referenced assets with blend mode + opacity. Style and pack
references are pinned by content hash, so a project re-renders identically on any
machine that has the packs.

### 5.4 History

Append-only command journal (`NdJSON`, one command per line, inverse recorded);
snapshots on close/every N ops. Every generator run and every accepted AI proposal is
just a batch of commands — undo, redo, and "show me what the AI changed" come free.
Autosave ≤ 2 s after any edit; a crash loses at most that.

---

## 6. Rendering pipeline and performance budgets

- **Viewport (ui/)**: WebGL2 (Canvas2D fallback) drawing 256 px tiles from
  `GET /tiles/{doc}/{floor}/{z}/{x}/{y}.webp` + a live vector overlay (selection,
  handles, proposals) on top. Pan/zoom never waits for the server; dirty tiles stream
  back over WebSocket and swap in.
- **Baker (ndcart-render)**: per-chunk replay through tiny-skia on a rayon pool
  (worker count mirrors nd-world's `ND_IMAGE_WORKERS` philosophy: half the cores,
  clamped), LOD pyramid z0..zN, deterministic (seeded; a bundled OFL font so text
  bakes identically on every OS). Incremental: a room edit re-bakes only its chunks
  (budget < 250 ms); a full 200×140-cell @ 32 px bake < 30 s on 8 cores.
- **Budgets (done-when numbers, measured by `ndcart bench`)**:
  - 60 fps pan/zoom with 100,000 objects across 4 LODs;
  - open-to-interactive on a saved 5,000-object project < 1.5 s;
  - export 12,800×8,960 WebP < 60 s;
  - memory < 1.5 GB with a 512×512-cell project open;
- **painting latency** — stroke feedback is GPU-composited in the viewport at once
  (< 1 frame from the pointer event); the authoritative Rust bake trails ≤ 250 ms
  behind; a 4k-px-wide material stroke across 100 chunks bakes < 1 s;
- **scatter at scale** — painting a forest writes a *density field* (one op); instance
  placement (10k+ stamps) renders from the field at LOD, never as 10k objects;
- **terrain scale** — heightmaps to 16,384² (tiled 16-bit, demand-paged per chunk);
  hillshade re-render of a dirty region < 500 ms.

If the WebGL tile pipeline ever ceilings (it should not at these budgets), the escape
hatch is a native wgpu viewport embedded per-platform — deliberately out of scope
until measured, same discipline the research doc applies to nd-world.

---

## 7. The studio: painting, tools, and generators

### 7.1 Battle-map painting (the Dungeondraft half)

- **Materials, not colors** — paint floors, water, roads, void, grass… as materials
  from the active pack; adjacent materials **blend automatically** (cliffs where floor
  meets void, shores where land meets water, road shoulders) — the signature
  Dungeondraft look, deterministic from seed, re-derivable at any zoom.
- **Brush engine** — round / stamp / pattern / scatter brushes; size, opacity, flow;
  pen **pressure and tilt** (Pointer Events); line / rectangle / fill variants;
  eraser and roughen (hand-drawn edge jitter).
- **Patterns** — procedural tile/brick/cobble generators with per-material scale,
  rotation, offset and color jitter: no tiling seams at any resolution, no fixed-size
  textures to ship.
- **Stamp placer** — tag/semantic search, arrays and mirror, scatter-with-collision,
  per-placement rotate/scale/flip, soft drop shadows, palette recolor.
- **Painted light** — colored light areas and glows as paint (for the *look*), beside
  the point lights that publish to nd-world and drive its fog (≤ 60 there).
- **Cave tools** — passage/blob painting with auto outline, chamfered so tokens never
  sit inside a wall **[tested pitfall, research §3]**.

### 7.2 Overland studio (the Canvas of Kings half)

- **Heightmap terrain** — paint elevation, water level, plateaus and valleys; the
  renderer derives hillshading, hypsometric tints, slope materials (cliffs on steep
  slopes, beaches at the waterline) and depth-shaded water.
- **Rivers that behave** — drawn upstream→downstream, they carve the heightmap and
  stay in valleys; lakes fill at a chosen level.
- **Biome brushes + global grading** — biome assignment by altitude/slope/moisture
  fields or by hand; Wonderdraft-style global tint and color grading per map.
- **Mass scatter** — forests, mountains, dunes, hamlets as *density fields*: paint
  *where*, the engine decides *what* and how many, instanced at LOD. A forest is one
  editable thing, not 8,000 objects.
- **Typography** — label styles (fonts, halos, small caps, letter spacing), **text
  along paths** (a river's name follows the river), collision-aware placement, region
  name plates; a bundled OFL font set plus user fonts.
- **Routes and journeys** — styled path lines with waymarks and day-marks, distance
  measure in world units.
- **Political regions** — bordered fills, hatching/pattern fills, nesting
  (nation → province → fief); publish to nd-world overlay regions.
- **Grid overlays** — hex (pointy/flat) or square, burned in or overlaid at export.
- **Hand-drawn style packs** — contour lines, hatching, sketchy stroke overlays,
  paper texture: a style is a swappable layer over the same data (§7.5).

### 7.3 Structure & battle-map tools

Select/lassoise/marquee + filter palette · room paint (drag rectangles, polygon
rooms, corridor stitcher with auto-connect and width) · edge marks (door / window /
secret / open) · walls derive from rooms **[research §3]** · point lights ·
measure/calibrate (units per cell, unit label — the same semantics as the in-app
grid dialog) · notes/secrets (markdown, `[gmonly]`-aware on publish) · floors
manager · **outliner** (floor → zone → room → object tree, drag-drop, multi-select
bulk restyle/relabel/move) · command palette (Ctrl+K, matching nd-world muscle
memory).

### 7.4 Generators

All seeded, deterministic, lockable — pin a region, re-roll the rest; unreachable
links are reported, never guessed **[research §3]**:

- dungeon (room-graph grammar → layout + furnishing recipes per room kind);
- cave (marching squares, chamfered outlines);
- building from a room graph (the "station wing" case);
- city district (road network + block grammar + plazas) — battle-scale blocks and a
  CoK-scale city stamp set come from the same plan;
- **overland** (seeded heightmap: coastlines, ridges, river networks, biome fields →
  terrain + scatter + label-ready regions).

Furnishing recipes place stamps by rule (no overlaps, clear doorways) — the prototype
furnished 14 rooms/76 props deterministically **[measured]**; the studio ships the
recipe system, not a fixed set.

### 7.5 Asset & art-pack ecosystem

- **`.ndpack`** — the open pack format (folder or zip + `manifest.toml`): materials
  with blend rules, stamps with tags/anchors/shadows, patterns, label styles, brush
  presets, fonts, and a license field. Packs are the unit of sharing between GMs
  (question 7).
- **Import**: Dungeondraft `.dungeondraft_pack` **[verify zip layout against a real
  file]**; Wonderdraft-style asset folders; generic PNG/JPG/WebP/AVIF/GIF/BMP/TIFF/SVG
  folder scans (nd-world's prop library accepts the same raster set **[verified,
  `app/routers/map_props.py`]**); `.dd2vtt` files open as projects (underlay +
  walls/lights/grid — the format is already read and written by nd-world
  **[research FORMATS]**); `.dungeondraft_map` project import **[verify]**.
- **Pack manager** — previews, enable/disable per project, license field surfaced at
  import (packs are the GM's licenses; the studio never redistributes them — D8).
- **Pack authoring in-app** — tag editor, auto-trim, shadow/anchor generation, blend
  rule tester; export a `.ndpack` to hand another GM.
- **From generation to pack** — AI-made materials and stamps (§8.5) save straight
  into a user pack with their provenance attached; sharing generated art is always
  the GM's explicit choice, never a side effect.
- **First-party starter packs (MIT)** — one battle-map set, one hand-drawn overland
  set — so the app looks like the competition on first launch, not like a wireframe.

**License posture (pack import, and Dungeondraft specifically).** Importing a file
*format* is not a license question: formats aren't copyrighted, `.dungeondraft_pack`
files are plain zip-of-PNG archives with no encryption or copy protection **[verify
at implementation]**, and reading an archive the user supplies is ordinary
interoperability — community converters between UVTT, Dungeondraft and Foundry have
existed for years (the research cites several, all permissively licensed). What
licenses *do* govern is the art inside: third-party packs carry per-pack terms (most
permit personal use of the art in any map tool; some restrict use to named
software), and Megasploot's own assets bundled *with* Dungeondraft/Wonderdraft are
licensed for maps exported from those apps, not as raw assets inside another editor.
The studio's rules, extending D8: never ship or redistribute pack contents; surface
each pack's license text at import and keep it with the project; name
Dungeondraft/Wonderdraft only factually ("imports `.dungeondraft_pack` files"),
never as endorsement; what a GM imports on their own machine is their call under
their own licenses. Not legal advice — during M5, re-read Megasploot's current EULA
and two or three real pack licenses, and record the wording in the new repo's docs.

---

## 8. AI integration

### 8.1 Principles (all inherited from nd-world's own patterns)

1. **The AI plans; deterministic code builds.** The model returns a *semantic* MapSpec
   — room kinds, sizes, links, lore refs, atmosphere — never raw coordinates
   (research's proven division: "AI chooses kinds, not coordinates"). Generators turn
   the spec into geometry; the GM sees a proposal, not a fait accompli.
2. **Proposals are command batches.** Ghost-rendered overlays; accept-all /
   accept-per-node / reject; everything lands in the journal, so undo and diffing are
   uniform.
3. **One GPU, one queue.** Connected mode routes every LLM/image call through
   nd-world's HTTP API so its `ai_queue` / semaphores stay the single arbiter (the
   cross-process `ai_slot` lease the Rust gateway already honors — `docs/RUST_JOBS.md`
   **[verified]**). The companion never calls Ollama directly while a server link is
   active.
4. **Long calls are background tasks.** The client sends `X-ND-Background: 1`, gets a
   202 + task id, polls `GET /api/ai/tasks/{id}` — the exact `ndAiFetch` contract
   **[verified, `app/ai_background.py:51-66,252-328`]** — so proxy timeouts and
   sleeping laptops can't kill a draft. The Rust client re-implements ~80 lines of it.

### 8.2 The MapSpec (canonical schema, shared both sides)

```jsonc
{
  "version": 1,
  "kind": "battle",                      // battle | region | schematic
  "floors": [{
    "name": "Ground", "cells": [200, 140],
    "rooms": [
      { "id": "r1", "kind": "ticket_hall", "size": [18, 12], "label_hint": "",
        "links": ["r2", "r5"], "furnish": "ticket_hall.default",
        "lore_entity": "ent_42", "notes": "collapsed east side" }
    ],
    "zones": [{ "id": "z1", "label": "Collapsed East Wing", "contains": ["r5"] }]
  }],
  "style": { "materials": ["concrete", "neon_tile"], "era": "post-industrial" },
  "terrain": { "sea_level": 0.42, "biomes": ["temperate", "marsh"], "ridges": [[120, 40]], "rivers": 6 },
  "labels_needed": true,
  "constraints": { "max_rooms": 30, "secrets": 2 }
}
```

Validated by a Rust `schemars`-derived JSON Schema + a `clean_map_spec` (unknown
`furnish`/`lore_entity` ids dropped with warnings, bounds clamped, `terrain` values
clamped to their ranges — the `template_draft.py` pattern; `terrain` applies to
`kind: "region"` only). The schema file is canonical in nd-cartographer and
vendored into nd-world if the Tier-1 route (§10.2) is built; shared fixture files +
round-trip tests keep them identical.

### 8.3 Features by phase

| Feature | Flow |
|---|---|
| **Draft from a prompt** | prompt → MapSpec → generator → proposal overlay → accept/partial. Room graph, not pixels: "abandoned metro station, 3 levels, flooded lower level, 2 secret routes" becomes a spec, then a map. |
| **Refine selection** | select rooms/zones → constrained re-roll (keep these, regen those, add a barracks) — same spec pipeline with pin lists. |
| **Semantic density masks** | the AI paints *where* (a low-res mask: forest here, swamp there, ruins clustered east) as part of a MapSpec; the scatter engine decides *what* — the AI never places thousands of objects, it paints intent. |
| **Overland from a prompt** | biome mix, sea level, ridge directions, river count → heightmap + scatter + label-ready regions, then refined conversationally. |
| **Furnish** | per-room recipes chosen by kind + lore; GM accepts per room. |
| **Label with the world's voice** | room/district/POI names grounded in nd-world RAG (`smart_world_context` filtered to `kind == "location"` entities **[verified, `app/retrieval.py:1208]**) so names agree with existing lore. |
| **Image-gen underlays** | region textures and painted-style underlays via nd-world's imagegen (`POST /api/ai/imagegen/jobs` with `width/height/steps/seed/init_image` **[verified, `app/routers/ai.py:2918-2945, 2663-2709`]**), fetched from `/uploads/ai-images/...` with the session cookie. Underlays are layers — AI images never overwrite geometry, so a bad generation costs nothing. **[verify server-side max dimensions]** |
| **AI assets: textures, objects, mini-packs** (§8.5) | the model doesn't only plan — it *makes the art*: seamless materials, cut-out prop stamps, style-consistent sets; every generation is previewed before it enters the library, provenance kept. |
| **Assisted tracing** (later) | imported/AI picture → proposed wall traces the GM snaps/approves. Naive detection is only 26–43 % precise **[measured]**, so this is explicitly an *assist*, never auto-apply. |
| **Standalone mode** | same MapSpec pipeline against a direct OpenAI-compatible/Ollama endpoint configured in-app; a local semaphore substitutes for the server queue. Icon and a banner make the mode obvious. |

### 8.4 AI routes used (all existing — verified)

- `POST /api/ai/chat` — non-streaming, `{"messages", "system", "model", "options",
  "think"}` → `{"result": text}` (`ai.py:401`); background-able.
- `POST /api/ai/imagegen/jobs` + `GET /api/ai/imagegen/jobs/{id}` — durable jobs,
  `{status, urls[]}` (`ai.py:2918-2945`); direct variant `POST
  /api/ai/imagegen/generate` for quick previews (`ai.py:2886`).
- `GET /api/ai/tasks/{id}` — poll shape `{status: queued|running|done, body}`.
- Prompt precedent to mirror: the in-app AI Build's strict-JSON system template
  (`main.py:7967-7974`) and its plan mode (`main.py:8361-8422`).

### 8.5 AI asset synthesis — textures, objects, mini-packs

The model doesn't only plan maps; it makes the art that paints them. All generation
goes through the same imagegen surface as underlays (nd-world's durable jobs when
connected, the local endpoint in standalone mode) under the same one-GPU queue
discipline — a 12-stamp set is 12 queued jobs with pacing and a Cancel, never a
burst.

- **Materials (textures)** — prompt → seamless tileable material. The backend
  generates; a Rust pass then guarantees what the model can't: wrap-around blending
  for perfect tiling, a derived height map (luminance) and normal map (Sobel) for
  the blending engine's edge profiles and AO, and palette matching to the active
  style profile. The result drops into the material palette like any pack material.
- **Objects (stamps/props)** — prompt → cut-out stamp: generated against a chroma
  background keyed out in Rust (deterministic, local — no cloud cut-out), or img2img
  from an existing stamp's silhouette; then auto-trim, anchor, soft shadow, optional
  recolor, and model-suggested tags the GM confirms. **[verify: cut-out quality per
  backend; an ONNX matting model is the planned upgrade if keying isn't enough]**
- **Mini-packs** — "rusty orbital crates, 12 variants, top-down": one style anchor
  (a reference image via init/ipadapter — the fields exist in `ImagegenBody`
  **[verified]** — or the style profile's tokens plus a shared seed family) keeps
  the set coherent; every item is previewed in a grid before anything enters the
  library.
- **Upscaling** — a 512² draft becomes a 2048² asset through the upscale/hires-fix
  parameters the server already exposes **[verified]**, run only once a candidate is
  a keeper (drafts are cheap, upscales are the queue's time).
- **The proposal discipline still applies** — nothing generated lands in the library
  or the document silently: a generation batch previews like a MapSpec proposal,
  is accepted per item, is undoable, and each kept asset records its provenance
  (model, prompt, seed, date) in the manifest. nd-world's CC-BY-NC-ND art is never
  used as an init image or training reference.
- **Reproducibility** — same prompt + seed + backend ⇒ the same asset while that
  backend exists; "regenerate" and "variant" are first-class actions; assets are
  content-hashed like any other, so maps referencing them re-render identically.

---

## 9. The nd-world bridge

### 9.1 Connection

1. `POST /api/login` with `{"email", "password", "totp_code"?}` — the route built for
   non-browser clients **[verified, `app/routers/auth.py:254-310`]**; keeps the signed
   `session` cookie (no CSRF layer exists; SameSite=lax; programmatic cookie use is
   fine **[verified]**).
2. Credentials in the OS keychain (keyring crate) on desktop; in Docker, a
   `ND_CARTO_ND_TOKEN_FILE` (email+password or a long-lived session export) under
   `/data` with 0600 — never a plain env var, never logged.
3. World targeting: send `?w=<slug>` on every request (cleanest; overrides the
   `active_world` cookie) **[verified, `app/deps.py:19-25`]**.
4. Capability probe at connect: `GET /health` (public) for build/mode, then HEAD the
   routes below; the UI shows "verified against nd-world ≥ <build>" or warns. Bridge
   sends `User-Agent: ndcart/<version>`.

### 9.2 Publish paths (verified routes only)

**A. Battle map → playable schematic (the headline path).** First publish:

1. `POST /maps/schematic/import-uvtt` — multipart `file` (.dd2vtt with the baked
   background) + `name` → `{"slug", "warnings", "stats"}` **[verified, `main.py:3500-
   3540`]**; creates the map with square grid, background, walls, lights in one call.
   The UVTT is generated by the companion (format proven in-repo; walls ≈ 5 KB, the
   picture is the cost).

Re-publish (updates the same map in place):

2. `POST /maps/schematic/{slug}/upload` — new baked background (multipart `file`).
3. `POST /maps/schematic/{slug}/walls` — `{"walls": [...]}` replace (validated,
   ≤ 5,000) **[verified, `map_walls.py:64`]**.
4. `POST /maps/schematic/{slug}/lights` — `{"lights": [...]}` replace (≤ 60)
   **[verified]**.
5. `POST /maps/schematic/{slug}/rooms` — room-grid state (keeps it re-editable in the
   in-app editor) **[verified, `map_walls.py:146`]**.
6. `POST /maps/schematic/{slug}/elements` — labels/decoration elements **[verified,
   `main.py:4218`]**. Stamp images: upload once per map via
   `POST /maps/schematic/{slug}/embed-image` (returns `{url}`) and reference that URL
   in `image` elements **[verified, `main.py:4260-4287`]**. **[verify href policy for
   cross-map reuse; fallback = embed per map]**
7. `POST /maps/schematic/{slug}/grid` — grid config `{cell_size, offset_x, offset_y,
   orientation, unit_per_cell, unit_label}` **[verified, `main.py:4035`]**; optionally
   `POST .../fog` for initial fog settings **[verified]**.
8. Optional: `POST /api/maps/props/upload` to add used stamps to the world's prop
   library for reuse **[verified, `map_props.py:92`]**.

**B. Region/world map → image map.**

1. `POST /maps/new` — form `name`, `width`, `height` (map units), `image_file`
   (multipart) → slug **[verified, `main.py:2790-2841`]**.
2. Overlays via `POST /api/import/execute` `{"json_text", "kind": "map_overlay",
   "params": {"map_slug": ...}}` — the importer **merges** (safer than the direct
   overlay route, which replaces) **[verified, `importer.py:670-715`]**; markers ≤ 500,
   regions ≤ 500.
3. Re-publish background: `POST /maps/{slug}/upload` (multipart `file`)
   **[verified, `main.py:2895`]**.

**C. Schematic diagram.** `POST /api/import/execute` `{"kind":
"schematic_elements", "params": {"schematic_slug": "__new__", "new_schematic_name",
"new_canvas_width", "new_canvas_height"}}` on first publish; the existing slug later —
merge-by-element-id, so re-publish updates cleanly **[verified, `importer.py:717-768`]**.
Detect-first (`POST /api/import/detect`) in tests, per `AGENTS.md`.

**Round-trip:** open a published map back in the studio — `GET
/maps/schematic/{slug}/walls.json` (all walls/fog/lights), `GET .../rooms.json`, `GET
.../view.json` (elements; GM viewers get everything) **[all verified]** — enough for
lossless-enough reopen without new server routes.

### 9.3 Size caps (the honest constraint)

Upload cap is 20 MB default, adjustable per instance (`AppSettings.max_upload_mb`)
**[verified, `app/uploads.py:21`, `app/models.py:2383`]**. The exporter therefore has
"fit for publish" presets: bake WebP q≈85 at 24–48 px/cell and auto-downscale until
under the *probed* cap (the bridge asks, it doesn't guess), with a warning and a link
to the setting when the GM wants full resolution. A 200×140 map at 32 px/cell is
6,400×4,480 ≈ 3–6 MB WebP — comfortably inside. Canvas bounds 100–20,000 px
**[verified]** bound the schematic canvas, not our document.

### 9.4 Standalone parity (a hard rule)

Nothing in the studio requires nd-world. With no server configured, the app is a
complete creator for any system and any table — UVTT / Foundry / PNG / PDF out, AI in
standalone mode (§8.3). The bridge is one module, compiled in but dormant until
configured, and CI runs the whole suite once with the bridge feature disabled to keep
that true (§11). Every feature review asks the question explicitly: *does this work on
a laptop in a cabin?*

---

## 10. nd-world-side changes

### 10.1 Tier 0 — required: **none.**

Every publish/update/read flow above uses routes that exist today. The companion is
buildable and shippable without touching nd-world.

### 10.2 Tier 1 — recommended small additions (each optional, additive, GM-gated)

| Addition | Why | Cost |
|---|---|---|
| `POST /api/ai/map-plan` (background task, `TASK_PATH_PATTERNS` + `@ai_queue.serialized`, GM-only): prompt + context digest → validated MapSpec (a `clean_map_spec` validator; generalizes the existing per-schematic ai-build plan mode) | Structured output server-side (Ollama format-schema), world RAG baked in, one place to tune prompts; both the companion and the in-app creator can call it | ~150 lines + tests, mirrors `template_draft.py` |
| `GET /api/maps.json?w=<slug>` — world's maps + slugs as JSON | Bridge world-browser and re-publish slug discovery without scraping HTML | ~30 lines |
| `GET /api/tables.json` + `GET /api/entities.json?kind=&q=` — read-only random-table and entity lists | Generators furnish rooms from the world's own tables; the lore browser and entity-grounded labeling work without scraping HTML | ~60 lines |
| Rows in `docs/API_REFERENCE.md` for all of them | Repo rule | docs |

Tier 1 lands as a normal nd-world PR (tests, golden gateway parity if the Rust
gateway must answer them too — check `rust/` coverage at implementation time).

**Tier 2 — alternative to the read APIs:** an ApiToken-driven **MCP read client**
(the `/mcp` surface is exactly what tokens exist for — search entities, character
digests) for AI context only, while publishing stays on the session cookie. Zero
nd-world changes, but it puts an MCP client into the companion. Open question 6
decides; the default is the Tier-1 read APIs.

---

## 11. Packaging, distribution, CI

### 11.1 Desktop

- **Windows**: Tauri v2 → `.msi` (WiX) + setup `.exe` (NSIS); WebView2 bootstrapped;
  portable `.zip` as the fallback artifact. `.ndmap` file association. Signing: no cert
  budget assumed — document the SmartScreen note instead.
- **Linux desktop**: AppImage + `.deb` from tauri-bundler, built on ubuntu-22.04 for
  glibc reach; webkitgtk dependency documented. `--browser` mode needs only the server
  binary (no GUI libs — the desktop crate is feature-gated out).

### 11.2 Docker (Linux)

Following the repo's image conventions **[verified]**:

- Multi-stage: `rust:1-slim` build (musl static for the server-only target — no GUI
  deps, so a ~2 MB-ish static binary like the gateway's) with the stub-`main.rs`
  dependency-layer caching trick from `Dockerfile.gateway` (no cargo-chef needed);
  runtime `debian:stable-slim` + ca-certificates + tzdata.
- `VOLUME ["/data"]` (projects, tile cache, config), `EXPOSE 8000`,
  `ENV ND_CARTO_LISTEN=0.0.0.0:8000`, public `GET /health` → `{"status":"ok",
  "version": ...}` (the nd-world `/health` shape).
- Auth: none on loopback by default; when listening non-loopback, require a token
  (first-run generated to `/data/`, printed once; or `ND_CARTO_TOKEN_FILE`). The
  nd-world session cookie the bridge holds is GM power — the server must not expose it
  unauthenticated on a LAN.
- Image: `ghcr.io/8bit-boom/nd-cartographer:{latest, <sha>}` — its own package, never
  a tag of an nd-world image (the Watchtower rationale from `docker-publish.yml:107-
  109`). Compose example alongside the nd-world stack.

### 11.3 CI (new repo, mirroring the family)

- `tests.yml`: `cargo fmt --check` + `clippy -D warnings` + `cargo test --locked`;
  `ui/` unit tests (vitest, Node-tested pure math — the repo's `test_nd_clipboard.py`
  ethos); bridge contract tests (wiremock, pinning every route in §9); bridge
  capability-probe test; `ndcart bench --smoke` budget assertions; one full pass with
  the bridge feature disabled (standalone parity, §9.4).
- `release.yml` (on tag): Windows + Linux desktop artifacts, Docker build gated on
  tests, ghcr push, GitHub Release with artifacts and a changelog. nd-world's rule —
  *the image builds only when the suite is green* — carries over.
- Optional nightly/weekly: live-integration profile that boots the real `nd-world`
  container and runs a publish round-trip against it (env-gated, like the golden
  tests' `ND_TEST_BASE_URL`).

---

## 12. Testing strategy

| Layer | Tests |
|---|---|
| `ndcart-core` | Unit + **proptest** determinism (same seed ⇒ byte-identical layout); walls-from-rooms oracle cases (ported from the research prototypes' 64-test suite); journal replay = snapshot equivalence; UVTT round-trip fixtures (a small synthetic `.dd2vtt` fixture checked against nd-world's own importer behavior; credit the BSD-3 source if the real sample file is vendored). |
| Schema parity | MapSpec JSON Schema validated against shared fixtures in both repos; a CI check that the vendored copies match. |
| `ndcart-render` | Golden tile hashes — geometry-only scenes plus one bundled font so Windows/Linux agree; LOD sanity; chunk-incremental bake equivalence. |
| `ndcart-paint`/`ndcart-terrain` | Stroke determinism (same op list ⇒ identical bake), material-blending golden tiles across platforms, heightmap algebra (carve-then-raise is defined), density-field ↔ instance-count properties. |
| Asset synthesis (§8.5) | seamlessness proven by construction (a tile must hash-equal itself offset by half — wrap-around blend is tested, not hoped for), chroma-key cut-out edge fixtures, provenance round-trip (generate → manifest → export → re-import), height/normal derivation golden images. |
| `ndcart-bridge` | wiremock contract tests per route (status/shape/merge semantics: importer map_overlay *merges*, overlay route *replaces*, schematic elements merge-by-id); 401 → re-login flow; capability probe against faked old/new servers; cap-probing/downscale logic. |
| TS/Rust parity | The TS preview's edge-rule/wall derivation consumes Rust-computed results over WS (no second implementation). Any math that must exist twice (viewport transforms) shares JSON fixtures, the way `combat-core.js` and `combat_turns.py` are tested on the same cases. |
| UI | vitest for pure functions; Playwright smoke on `ndcart serve` (open → paint → undo → export → publish-to-wiremock). |
| Perf | `ndcart bench` with the §6 budgets wired as CI assertions (like `scripts/perf_smoke.py`). |

---

## 13. Decision log

| # | Decision | Alternatives rejected (and why) |
|---|---|---|
| D1 | **Sibling repo `nd-cartographer`**, not a subdir of nd-world | Subdir couples releases to nd-world's image gate and grows the already-large CI; `CLAUDE.md:27` names the sibling repo as the home for the desktop editor. Communication is HTTP-only — no shared DB, no shared crates at first (copy the small validators; unify later if it hurts). |
| D2 | **Rust core + web UI + Tauri v2 shell** | *egui/iced native-only*: cannot honor the Docker deliverable (a GUI in a container is X11-forwarding or nothing); also discards the team's canvas-editor expertise. *Pure wgpu*: months of UI plumbing before the first room is drawn. *Electron*: not Rust, and the user asked for Rust. The hybrid keeps every correctness-critical line in Rust — and painting-grade input in a browser tab is proven territory (Figma, Photopea): strokes composite on the viewport GPU while Rust bakes authoritatively behind them (§6). |
| D3 | **AI through nd-world when connected** (queue etiquette, one config) | *Direct-to-Ollama always*: fights the single-GPU queue nd-world carefully maintains (cross-process `ai_slot` lease), and duplicates model config. Direct mode survives as the standalone fallback. |
| D4 | **Semantic MapSpec, not LLM coordinates** | Raw coordinate generation is what the in-app ai-build v1 did and what the research warns about; kinds+links validated then laid out deterministically is robust, cheap to retry, and matches the October-built plan mode. |
| D5 | **UVTT as the play-ready interchange** | A private "full map sync" format would need new nd-world routes on day one; UVTT is already imported by nd-world (and exportable to Foundry/Roll20 elsewhere), so Tier 0 stays at zero changes. The richer in-place updates then use the verified per-part routes. |
| D6 | **Chunked tiled document, LOD bake, sparse room grid** | Single-canvas documents (the in-app editor's model) are what caps it at ~6k objects; the tile approach is the same one nd-world itself uses for viewing (`app/map_tiles.py`). |
| D7 | **Walls derive from rooms** (edges between spaces) | Freehand walls inherit every hand-drawing mistake and can't be re-derived; the edge rule is integer, exact, recomputable **[tested]** — and it is already nd-world's own model, so published maps stay editable in-app. |
| D8 | **Pack compatibility, never pack redistribution** | The studio *imports* Dungeondraft/Wonderdraft packs (the GM's own licenses, surfaced in the pack manager) and ships only first-party MIT art; nd-world's CC-BY-NC-ND content stays out entirely (research decision §7.4). |
| D9 | **Standalone-first product; nd-world as flagship integration** | *Companion-only scope* (round one): undersold the tool and stranded it without the server — replaced on the owner's direction. *nd-world thin client*: not a map creator at all. Standalone-first keeps every feature honest (the §9.4 parity rule) while §9's publish depth remains the differentiator no other map tool has. |

---

## 14. Milestones

Estimates: one developer with AI assistance, focused days, ±50 % (the research doc's
own calibration; production = prototype × 3–5). The standalone-first revision grows
the earlier companion-only estimate; the order is chosen so that **every milestone
ships a usable standalone app**.

| M | What | Est. | Done when |
|---|---|---|---|
| **M0 — Skeleton** | Repo bootstrap (workspace, CI, license files), core doc + journal + sparse grid, tile baker + `/tiles`, WebGL viewport (pan/zoom/select), Tauri shell + `serve` + Docker image + `/health`, `ndcart bench`; the studio's own HTTP/WS contract documented from day one (§17.6) | 1.5–2 wk | 100k-object project pans at 60 fps; kill -9 mid-edit loses ≤ 2 s; the Docker image opens in a browser |
| **M1 — Battle-map studio** | **Painting engine v1** (round/pattern/scatter brushes with pressure + tilt, materials with automatic blending, eraser/roughen), stamps + drop shadows, rooms/corridors/edge marks + walls-from-rooms, point lights, undo/redo, outliner v1, keyboard-help overlay, **linter v1**, UVTT import/export (round-trip test), PNG/WebP + player/GM split exports with cap fitting, **publish A** + multi-world pinning + slug capture, first-party battle pack v1 | 5–7 wk | a GM paints a good-looking 200×140 dungeon with the starter pack, publishes it to nd-world *played* (fog + tokens), re-publishes in place; a Dungeondraft `.dd2vtt` opens and re-publishes; the Problems panel catches a deliberately broken map |
| **M2 — Overland studio** | **Heightmap terrain engine** (paint elevation/water; hillshade, hypso, slope materials; carving rivers, lakes), biome brushes + global tint/grading, **mass scatter** (density fields + instanced LOD stamps), **typography** (styles, text-along-path, collision), routes/journeys, political regions with pattern fills, hex/square overlays, overland generator v1 (seeded coast/ridge/river/biome), **publish B**, first-party overland pack v1 | 6–8 wk | a Canvas-of-Kings-class regional map (continents, named rivers, 300+ labels, political regions) painted start to finish, exported huge, published as an nd-world image map with 200+ overlays |
| **M3 — Structure & scale** | Multi-floor + onion skin, zones, bulk ops + select-by, symmetry mode, prefab library + project wizard, named checkpoints + history browser, project gallery, minimap + camera bookmarks, cross-project paste, generator full set (dungeon/cave/building/city/deck), **batch publish** + report/rollback, copy-view-as-image, linter full set, live-refresh awareness | 5–7 wk | a city district and a 3-floor station generated, edited and published (the district as an image map with 200+ overlays); the §6 budgets hold |
| **M4 — Play & AI** | **Player-visibility preview** + publish gate, MapSpec proposal workflow (ghost/partial/diff), conversational refine sidebar, furnishing (optionally from the world's own random tables, Tier-1), entity-grounded rooms, lore-grounded labels, **semantic density masks**, overland prompt→terrain, imagegen underlays, **AI asset synthesis** (§8.5: seamless materials, cut-out prop stamps, style-consistent mini-packs), ask-your-maps, style profiles, stamp auto-tag, standalone direct mode, background-task client | 5–6 wk | "abandoned metro station, 3 levels, flooded bottom" *and* "a cold archipelago kingdom, three political regions" prompts → acceptable proposals in < 2 min; a generated material and a generated prop stamp paint like first-party assets, with provenance kept; every AI change visible, rejectable, undoable; the queue is never bypassed |
| **M5 — Ecosystem & 1.0** | **Art-pack import** (`.dungeondraft_pack`, Wonderdraft folders, generic scans) + pack manager/tinting + pack authoring, Foundry scene export **and import**, PDF/print export, **table remote** + TV deep link, schematic-diagram mode + **publish C**, round-trip reopen, keychain/settings UI, `ndcart doctor`, docs (`ARCHITECTURE`, `AI_CARTOGRAPHER_GUIDE`, `MAP_SPEC`, `PACK`), perf pass, installers, first tagged release; optionally Tier-1 nd-world routes | 4–6 wk | a friend installs on Windows, imports their Dungeondraft pack, paints a map, prints it at one-inch squares, and publishes to nd-world following only the README |

Total ≈ **27–36 focused weeks** to 1.0 (standalone-first scope; P2 items from §17
stay backlog).

---

## 15. Risks

| Risk | Mitigation |
|---|---|
| WebGL/Canvas viewport ceilings on huge docs | tile pipeline + LOD from day one; §6 budgets in CI; wgpu-native escape hatch documented but gated on measurement |
| nd-world upload cap (20 MB default) blocks big publishes | cap probe + auto-downscale presets + clear guidance to raise `max_upload_mb` (it exists for this) |
| UVTT dialect drift (Foundry v14, DA variants) | fixture-driven converters + import warnings report (nd-world's proven approach); re-validate before each release |
| AI hallucination in specs | schema + `clean_map_spec` + bounds + retry-once; proposals never auto-apply; semantic specs make failures cheap |
| Rust/TS logic drift | single source of truth server-side; shared fixtures for what must be duplicated |
| Holding GM credentials in the companion | keychain at rest; token-gated server off loopback; no logging; long-lived session preferred over storing the password where possible **[verify session lifetime]** |
| Scope creep ("full-blown has no end") | every milestone has done-when; interchange imports what we don't build; no art competition |
| Tauri/webkitgtk quirks on Linux | `--browser` mode ships in the same binary as the unconditional fallback |
| License contamination | no nd-world CC content; CI lint on the assets dir; first-party MIT art only |
| Painting-engine complexity (the Dungeondraft half is the deepest tech in the app) | stroke feedback GPU-composited in the viewport, never waiting on the baker; material blending behind a narrow, heavily-tested interface; M1 proves the core loop before the overland engine invests on top of it |
| Art scope creep ("full-blown" again — Dungeondraft itself took years) | every milestone ships usable standalone value; pack import covers look gaps the first-party art doesn't reach; §17.9 stays enforced |
| AI-generated asset quality/consistency and provenance | preview-before-library (a generation batch is a proposal); style profiles + shared seed families for set coherence; per-asset provenance (model/prompt/seed/date) in the manifest; packs containing generated art exported only by explicit choice, flagged as such |

---

## 16. Open questions (answers shape milestones, none block M0)

1. **Art direction** — first-party procedural starter packs, or lean on imported
   Dungeondraft/Wonderdraft packs for the look? [default: both, on their milestones —
   first-party starter sets in M1/M2 for out-of-the-box quality, pack import in M5
   for the ecosystem, AI imagegen underlays in M4]
2. **Region-map depth in v1** — Canvas-of-Kings parity is deep (hand-drawn styles,
   huge pin counts); is M2's v1 (zones/roads/labels + AI underlay) the right cut?
   [default: yes]
3. **Tier-1 nd-world routes** — wanted in nd-world proper (they'd also serve the in-app
   creator), or keep nd-world untouched for now? [default: propose after M1, when the
   companion proves the chat-based path works]
4. **An MCP surface for the studio** (`ndcart mcp`: create_map_from_spec, bake,
   export) so agents can drive it — worth scheduling in M4+? [default: backlog]
5. **Multi-GM editing** — even a crude "project is open elsewhere" lock, or nothing?
   [default: lock file + warning, nothing more]
6. **AI context source** — Tier-1 read APIs (tables/entities JSON, §10.2) or a
   Token-driven MCP read client (no nd-world changes, but an MCP client in Rust)?
   [default: Tier-1 read APIs; MCP client only if those additions are unwelcome]
7. **Sharing between GMs** — plain `.ndmap`/pack files exchanged by hand
   (license-safe assets only), or is a community hub wanted at some point?
   [default: files by hand; no hub in 1.x]
8. **Asset-generation reference backend** — the studio must look good on the GM's
   actual image backend (ComfyUI / SwarmUI / Unsloth / direct endpoint); which one is
   the design target for material and stamp quality? [default: design against the
   nd-world-configured backend, degrade gracefully, and pin one known-good model id
   in the docs]

---

## 17. Round two — additional features and improvements

A second design pass over the plan. Every item is tagged **[P1]** (already folded
into the milestone table above) or **[P2]** (post-1.0 backlog); tags track §14's
current (standalone-first) milestone letters. nd-world hooks cited are verified
unless marked **[verify]**.

### 17.1 Authoring & editor

- **Symmetry editing** — live X/Y/point mirroring of paint, rooms and stamps (the
  "two identical wings" case, drawn once). [P1, M3]
- **Select-by attribute** — material, kind, tag, AI label; grow/shrink; select-same-
  stamp; pairs with the M3 bulk tools. [P1, M3]
- **Corridor tool with width + auto-door heuristics** (door where a corridor meets a
  room, never mid-wall). [P1, M1]
- **Room roles** — kitchen/barracks/atrium/airlock/… drive furnishing recipes, label
  icons and AI vocabulary; a role is data, not code. [P1, M3]
- **Onion-skin underlay** — the floor below rendered faintly under the current one,
  so stairs line up and levels read as one place. [P1, M3]
- **Minimap + camera bookmarks** — saved views of the document (per user, like
  nd-world's saved views). [P1, M3]
- **Named checkpoints + history browser** — the journal already records everything;
  this is UI over it ("state before the AI pass", "session 12"). [P1, M3] Visual
  diff between two checkpoints: [P2]
- **Copy/paste across projects** — placements carry their assets, deduplicated by
  content hash. [P1, M3]
- **Keyboard help overlay** — ONE table is both the handler and the F1 help (the
  repo's `cockpit-keys.js` rule, so they can never drift). [P1, M1]
- **Stamp effects** — drop shadows land in the M1 baker; palette recolor (hue-shift
  a whole imported pack to match a world) and baked ambient occlusion come with the
  M3 theming work. [P1 — shadows M1, recolor/AO M3]
- Spline/freehand rivers and roads for region maps. [P1, M2]
- Zen mode and a local present view (the studio's own GM view on a second monitor).
  [P2]
- Isometric render mode. [P2 — explicitly parked until asked for]

### 17.2 Map quality: the linter and the true preview

The feature that makes *complex* maps trustworthy — at 300 rooms nobody sees
mistakes by eye.

- **Problems panel (deterministic, no AI, IDE-style)**: unreachable rooms
  (connectivity over doors/corridors — the generators' "report, never guess" rule
  applied to hand edits), doors/windows opening into void, no entrance anywhere,
  darkness on with unlit key rooms, walls over nd-world's 5,000 cap / lights over
  60 (publish would be rejected — catch it first), grid misalignment, rule-based
  stamp collisions, degenerate geometry, GM-only text that a player-targeted export
  would leak. Every problem zooms-to and offers a fix *as a normal undoable
  command*. Core set in M1, full set in M3. [P1]
- **Player-visibility preview** — a local render of exactly what the table sees:
  the fog sweep, darkness and light radii, secrets stripped. The visibility
  algorithm is proven in-repo (0 disagreements with an independent oracle over
  202,500 points **[measured, research §3]**) and its JS implementation exists to
  port; parity fixtures are shared with nd-world's so preview = truth. [P1, M4]
- **Publish gate** — the linter runs before any publish; warnings are dismissible
  only through an explicit "publish anyway". [P1, M4]

### 17.3 Deeper nd-world integration

- **One-shot batch publish** — mixed content in a single
  `POST /api/import/execute` `{"imports":[...]}` call (≤ 200 items, per-item
  results **[verified, `importer.py:880-905`]**): overlays + schematic elements +
  new Location entities for freshly labeled POIs (via `entity_bulk`) land in one
  round trip instead of a dozen. [P1, M3]
- **Entity-grounded rooms** — rooms carry `lore_entity` ids from MapSpec; publish
  writes them into labels/notes so nd-world's in-app lore links and AI context pick
  them up. **[verify: exactly how the in-app room↔entity links are stored, then
  align]** [P1, M4]
- **World tables as generation input** — room contents (encounters, treasure,
  rumors) drawn from the world's own random tables via Tier-1 `GET
  /api/tables.json`: the world's tables keep working inside the studio. [P1 with
  Tier-1, M4; degrades gracefully without it]
- **Stamp auto-tagging** through nd-world's auto-tag routes when connected.
  **[verify: the routes' applicability to arbitrary stamp images]** [P1, M4]
- **Publish report + rollback** — the manifest keeps the previous published state
  hash; a re-publish reports exactly what changed (walls/elements/lights deltas),
  and rollback re-publishes the prior state — including `POST /maps/{slug}/delete`
  for maps the studio itself created. [P1, M3]
- **Table remote** — a small always-available panel of runtime toggles a GM can
  press from the studio mid-game: door open/close, light on/off, fog reset/epoch
  (all verified `map_walls.py` routes), plus a deep link to TV mode
  (`/display/map/{slug}` **[research, built]**). The studio stays a remote;
  nd-world remains the table. [P1, M5]
- **Live-refresh awareness** — those routes already call `live.touch` on write
  **[verified]**, so after a publish the studio shows "players' views refreshed"
  and can thumbnail the player view (`view.json`) back at the GM. [P1, M3]
- **Multi-world pinning** — a project is pinned to one world; switching re-targets
  everything through `?w=`. [P1, M1 — trivial]
- **Copy view as image** — the current viewport (or a selected region) to the
  clipboard as a picture, ready to Ctrl+V straight into nd-world's session-media
  and upload panels (their shared paste handler hands it to the visible upload
  field — `nd-clipboard.js` **[verified convention]**). A rendered fragment
  becomes a handout in two keystrokes. [P1, M3]
- Session Log ties (which maps a session used), quest/calendar cross-links,
  ambient-sound zone metadata (exported into region notes; nd-world has no map-
  audio runtime link today). [P2]
- Offline publish queue — queue publishes while the server is away, drain on
  reconnect. [P2]

### 17.4 AI, extended

- **Conversational refine sidebar** on `POST /api/ai/stream` (SSE; a GM always
  passes its gate **[verified, `ai.py:1522-1615`]**): "flood the lower level",
  "move the barracks east" → a *targeted MapSpec diff* proposal, not a redraw.
  Streams are deliberately never queued in nd-world, so the sidebar falls back to
  the background-task path when the GPU is busy — same proposals, calmer plumbing.
  Stop button always works (their rule for streams). [P1, M4]
- **Ask-your-maps** — local full-text search across every project's rooms, zones
  and notes ("which map had the flooded cistern?"), zoom-to-result. The index is
  cheap; finding things again is the whole point of a library. [P1, M4]
- **Style profiles** — named material/palette/era profiles kept per world ("this
  station's look"), included in MapSpec `style`, so floors and sister maps stay
  visually coherent and imagegen prompts stay on-brand. [P1, M4]
- **Roster-aware pacing** (optional) — party digest as context for encounter-
  density suggestions, via open question 6's context source. [P2]
- **Vision critique** — an optional, clearly-labeled model opinion of the current
  view ("the east wing reads empty"). Never a gate, never auto. [P2]
- **True region inpainting** — mask-based imagegen for terrain patches.
  **[verify backend mask support]** [P2]
- Assisted tracing stays later-phase exactly as §8.3 says. [P2]

### 17.5 Output & interchange

- **PDF/print export** — tiled pages, one grid square = one inch (the battle-map
  print standard), crop marks, GM and player variants; `lopdf` is already a family
  dependency (nd-gateway ships it **[verified]**). GMs print maps; no VTT at the
  table should not mean no map. [P1, M5]
- **Player/GM split exports** — secrets and GM notes stripped by the *same* filter
  the preview uses (§17.7's one-filter rule). [P1, M1]
- **Variant exports** — day/night/pre-fire as named layer-sets; each exports as its
  own image; publishing a variant as a `-night`-suffixed map. [P1 export, M3;
  variant publishing P2]
- **Foundry scene import** (the reverse of export; the format knowledge is in-repo
  from the export work). [P1, M5]
- **Watabou city JSON import** as a region-map seed. **[verify format]** [P2]

### 17.6 Library, templates, operations

- **Prefab/template library** — save any room, wing or floor as a self-contained
  prefab (assets by hash); new-project wizard (dungeon/city/deck/region) with an
  optional AI brief interview that ends in a MapSpec. [P1, M3]
- **Project gallery** — a catalog with auto-generated thumbnails (the low-LOD
  tile), tags, world links, last-published state. [P1, M3]
- **First-party material packs** beyond the starter sets (neon-industrial, grim-
  medieval, …) as MIT content, released on their own cadence. [P1 starter sets,
  M1/M2; more packs over time]
- **`ndcart doctor`** — verify assets, rebuild caches, validate/repair the journal,
  report orphaned content hashes. [P1, M5]
- **Studio API docs** — the studio's own HTTP/WS surface is a product contract
  (desktop shell, browser mode and the CLI all sit on it, and the future MCP
  surface is exactly this API); documented and versioned from M0. [P1]
- `ndcart watch` — folder-watch automation (drop a spec, get exports/publishes);
  headless recipe scripts for AI pipelines. [P2]

### 17.7 Security & robustness (threat model — research §5.6 mirrored)

- **Every number that crosses a boundary is validated**: finite, typed, bounded —
  journal commands, UVTT/Foundry/Watabou imports, published JSON. The D1 lesson (a
  `NaN` once bricked a live map for everyone) applies to the studio's own files and
  to everything it sends to nd-world.
- **Import bombs**: caps on JSON bytes/depth, polyline points, wall counts (mirror
  nd-world's 5,000 walls / 60 lights), and an image pixel budget via the image
  crate's decoder limits (the Pillow-limit equivalent).
- **SVG stamps sanitized once at import** with an allowlist (nd-world's hardened
  prop-SVG rules are the model); exports reference URLs, never inline user SVG.
- **One secrets filter** — the same strip function renders the player preview, the
  player-targeted exports and publishes (the research's "one `walls_for_viewer`
  used by everything" rule); a payload test diffs what each emits.
- **Supply chain**: `cargo deny` (licenses + advisories) in CI, `cargo audit` on
  release, vendored UI deps with license files.
- **Credentials**: keychain at rest, excluded from logs and crash dumps,
  token-gated server off loopback (§11.2). No telemetry; crash reports stay local.
- **AI-generated assets carry provenance** — model, prompt, seed and date recorded in
  the asset's manifest entry; a pack exported with generated art says so plainly, so
  nobody unknowingly redistributes model-limited images; nd-world's CC-BY-NC-ND art
  is never an init image or style reference.

### 17.8 Accessibility

- **The outliner is the accessible map**: a keyboard-complete tree view with
  screen-reader labels — every command reachable from it, the canvas a bonus
  rather than the only path. A map is a structure; the structure is already text.
- High-contrast and reduced-motion themes honored in the viewport (fog fades,
  proposal animations); focus outlines never suppressed; target sizes and keyboard
  model follow the discipline the repo already applies (cockpit keys, hub phone
  bar).

### 17.9 Explicitly not doing (so the scope stays honest)

A community pack hub (open question 7 — packs are files exchanged by hand);
real-time collaboration; general raster/photo editing (curves, masks, photo
filters — painting here is constructive: materials, stamps, heightmaps); isometric
rendering in 1.x; i18n in 1.x; Inkarnate import (no open format); any runtime play
feature beyond the table remote — nd-world owns the table.

## 18. Execution checklist (original; progress noted)

1. Create the sibling repo `nd-cartographer` (private first), with this plan adapted
   into its `README.md` + `docs/ARCHITECTURE.md`; MIT `LICENSE`; CI skeletons from §11.
2. `crates/` workspace + `ui/` vite skeleton; vendored first dependency with its
   license file; `cargo deny` / `about` for the license inventory.
3. M0 spike in order: core doc → baker → viewport → shell/container; `ndcart bench`
   wired to CI before any feature work.
4. Re-verify every **[verify]** item in this plan against the live nd-world build
   (href policy, imagegen max sizes, session lifetime, `.dungeondraft_map` and
   `.dungeondraft_pack` layouts against real files, rooms-state caps).
5. Fixture set: synthetic `.dd2vtt`, MapSpec fixtures, golden tile scenes, wiremock
   nd-world (login, import-uvtt, walls/lights/rooms/elements/grid/upload, importer
   kinds, overlay).
6. Then M1 as specced, publishing into a local nd-world container from the first week
   — the publish path is the product.
7. Seed the new repo's issue tracker from §17: the P1 items are already in the
   milestone table above; every P2 item becomes a labeled backlog issue so nothing
   in the round-two pass is lost.

---

## 19. Review pass (2026-11-05) — corrections and re-ordering

Found by re-reading this plan against the M0 code and the current nd-world tree.
These supersede the text above where they conflict.

**Corrected nd-world facts**
1. **The Rust-only nd-world image does not serve `POST /maps/schematic/import-uvtt`**
   (Python only; the gateway has walls/lights/rooms in `edits.rs`). Publish A must
   probe `/health` (`ND_MODE`, `ND_PYTHON_FREE`) and the routes it needs, and fall back
   to the per-part path (create map → background upload → `/walls` → `/lights` →
   `/rooms` → `/grid`) — or port `import-uvtt` to the gateway as Tier-1 work. Every
   other route in §9.2 must be checked against the gateway the same way.
2. `import-uvtt` **refuses a file with no picture**, so a full-map render is a
   prerequisite of publish A, not a later export feature.
3. nd-world's limits are the studio's limits: light range ≤ 60 squares (the core
   allowed 200), 5,000 lines / 20,000 points per UVTT import, every UVTT portal
   imports as a door (windows are not distinguished). The linter flags all of these.
4. Cite routes by name, not by `file:line` (they drift); §18.4's re-verification is
   mandatory before publish work starts.

**Re-ordered milestones**
- **M1 splits.** *M1a — structure & publish* (rooms, marks, walls-from-rooms, full-map
  PNG export, UVTT in/out, publish A against a local nd-world, linter v1) comes first
  because §18.6 says the publish path is the product. *M1b — painting* (brushes,
  materials with blending, stamps + shadows, starter pack) follows once publishing is
  proven.
- **Hex battle maps are out of v1**: walls-from-rooms, nd-world's room grid and UVTT
  are square-only. Hex stays a region-map overlay (M2).
- **Tauri shell moves to M5** with the installers; browser mode covers everything
  until then. M0's remaining items (WebGL viewport, WS change feed, bench budgets in
  CI, the 100k-object and kill -9 tests) are tracked in ARCHITECTURE.md.

**More verified nd-world facts (read from the handlers, then confirmed live)**
- `POST /maps/schematic/{slug}/grid` takes `{"grid_type": "square", "config":
  {cell_size, offset_x, offset_y, unit_per_cell, unit_label}}`, not the flat body
  §9.2 shows. `POST /maps/schematic/new` is a form post answering `303` with the
  slug in `Location` (it also needs a 100..20,000 px canvas).
- The Rust-only gateway *does* serve `/new`, `/upload`, `/embed-image`, `/grid`,
  `/walls`, `/lights`, `/rooms`, `/elements` (`edits.rs`); only `import-uvtt` is
  Python-only. Publish A therefore uses the per-part routes on every image.
- Background upload accepts WebP and AVIF as well as PNG/JPEG, so a publish can
  send the smaller formats.
- nd-world's room state is not a pass-through: space ids are integers 1..9999,
  kinds come from a fixed list, cells are run-length coded, and an edge mark names
  the cell whose top (`h`) or left (`v`) edge it is.

**Verified live on both images** (a real nd-world in Python mode, and the Rust-only
`nd-gateway` built from the current tree with `ND_PYTHON_FREE=1`, no Python behind it,
on throwaway data): create → picture → grid → walls → lights → rooms, then an in-place
re-publish with an AVIF background, succeed on both and read back correctly on Python.
**One gap on the Rust-only image:** `GET /maps/schematic/{slug}/walls.json` and
`rooms.json` answer `501`; only `view.json` is served. So §9.2's "reopen a published
map" works against Python/hybrid but not Rust-only. Publishing is unaffected (the
studio keeps its own document); the probe reports `read_back`. Closing it is a Tier-1
nd-world change (port those two GETs to the gateway) — noted, not done.

**Order change (owner go-ahead):** split exports → outliner → materials/starter pack →
brush engine. The starter pack moved *behind* the material system it is made of, so
M1b opened with `ndcart-paint` (materials, procedural patterns, edge blending) and the
first-party procedural pack; the brush engine followed: strokes as journal ops with pressure/tilt, round and
scatter brushes, an eraser that reveals the room layer, all deterministic and
tile-identical (see ARCHITECTURE). Stamps with drop shadows remain.

**Resolved:** wall ids are now stable (derived from the edge), so a re-publish keeps nd-world's runtime door state for any door that has not moved.

**Publish B facts (read from the handlers, then confirmed live on both images):**
- `POST /maps/new` is a multipart form (`name`, `width`, `height`, `image_file`) that answers
  `303 /maps/{slug}`. It **silently ignores a picture whose extension is not allowed** and makes
  a map with no image, so the bridge refuses unsupported types before the first request.
  `POST /maps/{slug}/upload` swaps the picture in place (WebP and AVIF are accepted).
- **The plan was wrong that the importer "merges" overlays**: `map_overlay` *appends*
  (`markers.extend`), so re-publishing through it would duplicate every marker. Overlays are
  therefore set with the replace route `POST /api/maps/{slug}/overlay`
  (`{custom_markers, custom_regions}`, ≤ 500 each), which both images serve. There is no JSON
  read route (the viewer inlines the overlay into the page), so a replace discards markers added
  by hand in nd-world; `ndcart publish --keep-overlay` skips it on a re-publish.
- Coordinates are Leaflet `CRS.Simple` over `[[0,0],[height,width]]`: `lat` counts up from the
  bottom of the picture (`lat = height_px - y_px`), `lng` is `x_px`. Regions are
  `{points:[[lat,lng],…], name, color, opacity}`. nd-world has **no route overlay**, so routes
  appear in the baked picture only.

**Image formats** — exports support PNG, WebP and AVIF (owner request). UVTT pictures
stay PNG/WebP (the consumers can't read AVIF). Lossy WebP for cap-fitting is a
follow-up (needs libwebp); until then cap-fitting uses AVIF for plain exports and
downscaling for publish.

**Hardening pulled forward (done in M0+)**
- Token gate when listening off loopback (§11.2) — implemented.
- Crash-safe saves (temp file + fsync + rename) — implemented.
- Doc/plan drift: the code uses `manifest.json`, PNG tiles at `{x}_{y}`, port 8300;
  the plan's `manifest.toml` / `.webp` / port 8000 are the eventual `.ndmap` shape.

---

## 20. Where the work stands

| Milestone | State | Built | Still to do |
|---|---|---|---|
| **M0 skeleton** | done except three items | document model, append-only linear-history journal, sparse room grid, walls-from-rooms (stable ids), tile baker, browser viewport, token gate, atomic saves, Docker, `bench` | WebGL viewport (Canvas2D today), WebSocket change feed, the 100k-object/60 fps and kill -9 acceptance tests; Tauri shell is parked for M5 |
| **M1 battle-map studio** | **done** | materials + edge blending, brush engine (round/scatter, pressure/tilt, eraser), stamps with shadows, doors/windows, linter v1, outliner v1, split exports, UVTT in/out, publish A (verified on the Python app and the Rust-only gateway), first-party starter pack, PNG/WebP/AVIF | lossy WebP (needs libwebp), pattern-stamp brushes and fill/line/rect variants |
| **M2 overland studio** | **mostly done** | heightmap terrain (sparse chunks, brush + river carving, exact undo), shading with global grading, seeded world generator, markers/regions/routes, **typography with text along paths and collision handling**, publish B (verified on both images), world-map UI | mass scatter (density fields), biome brushes, hex/square overlay, spline rivers/roads |
| **M3 structure & scale** | in progress | — | **done:** multi-floor (floor id per journal entry, structure history, floor bar, onion skin). per-floor publish (`--all-floors`), stairs links. symmetry for room painting (mirror left/right and up/down about chosen axis lines; strokes, stamps and edge marks do not mirror yet). zones (named room groups, `upsert_zone`/`remove_zone`), multi-select + select-by (material/kind/colour/zone/invert) with bulk colour/kind/prefix/delete as one undo step. named checkpoints (save / restore / delete; a restore saves the state it replaces and restarts undo history). generators for battle maps (dungeon, cave, building, ship deck, city district: seeded, one undo step, `POST /generate-map`, floor-bar dialog). linter extras (stale/empty zones with fixes, repeated room names, dead-end corridors, rooms reachable only through a secret door). batch publish (`ndcart publish --all-docs [--all-floors] --report FILE [--keep-going]`: everything is built and linted before the first request, empty floors are skipped, slugs are saved after each map, re-runs update in place; verified live on the Python nd-world. There is no rollback because nd-world exposes no map-delete route — the report lists every slug so the GM can remove them by hand). prefab library + cross-project paste (save selected rooms with their doors/lights/notes, place anywhere, rotated, one undo step; stored in `<data dir>/prefabs/`). **left:** prefabs, checkpoints, project gallery, generators (dungeon/cave/building/city), batch publish, full linter |
| **M4 play & AI** | not started | — | player-visibility preview, MapSpec + proposals, LLM/imagegen clients through nd-world's queue, AI asset synthesis, lore grounding |
| **M5 ecosystem & 1.0** | not started | — | art-pack import, Foundry export/import, PDF/print, table remote, `ndcart doctor`, installers, docs set, release |

Known costs worth knowing before M3: `save_doc` rewrites the whole journal on every op (a
terrain-heavy document grows MBs of undo snapshots; the append-only segment format planned for
`.ndmap` fixes it); prepared strokes are rebuilt per tile; underlays are bilinear without
mip-maps. The Docker image has not been built since the AVIF/TLS/terrain crates were added.

---

*Plan written 2026-11-05. Sources: `AGENTS.md`, `CLAUDE.md`, `docs/MAP_CREATOR_RESEARCH.md`
(+ its appendices), `docs/AI_SCHEMATIC_GUIDE.md`, `app/main.py`, `app/routers/map_walls.py`,
`app/routers/map_props.py`, `app/routers/importer.py`, `app/routers/ai.py`,
`app/routers/auth.py`, `app/deps.py`, `app/uploads.py`, `app/models.py`,
`app/ai_background.py`, `app/ai_queue.py`, `app/retrieval.py`, `app/map_tiles.py`,
`rust/Cargo.toml`, `Dockerfile*`, `.github/workflows/*` — all read in this working tree.*
