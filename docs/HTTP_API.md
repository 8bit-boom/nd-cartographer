# nd-cartographer HTTP API

The studio's own HTTP surface is a product contract (the desktop shell, browser
mode and the CLI all sit on it; the MCP surface wraps the same calls — plan
§17.6). This file documents what exists and evolves with the code.

Everything is JSON unless noted. Loopback listeners are open but reject a
non-loopback `Host` header (DNS-rebinding guard); non-loopback listeners require
a token (see [Access control](#access-control)).

## Health

| Method | Path | Notes |
|---|---|---|
| GET | `/health` | `{"status":"ok","app":"nd-cartographer","version":"…"}` — public |

## Documents

| Method | Path | Body | Notes |
|---|---|---|---|
| GET | `/api/docs` | – | `{"docs":[{id,name,kind,rev,floors:[names],slug?}]}` |
| POST | `/api/docs` | `{name, kind?}` | kind: `battle` (default) / `region` / `schematic`. Returns the `Manifest`. Creates floor "Ground". |
| GET | `/api/docs/{id}` | – | `{"manifest", "rev", "floors":[FloorView]}` |
| PATCH | `/api/docs/{id}` | `{name}` | Rename the project (1–120 chars). `{"ok":true,"id","name"}`. |
| DELETE | `/api/docs/{id}` | – | Delete the project and its files. `{"ok":true,"deleted":id}`. 404 if unknown. The UI confirms first. |
| GET | `/api/docs/{id}/save-status` | – | Auto-save state: `{rev, saved_rev, saved_at, dirty, persisted}`. `dirty` = edits not yet durably on disk; `persisted` = false when running without a data folder. |
| POST | `/api/docs/{id}/save` | – | Force a durable save now (flushes past the fsync throttle). Returns the same shape as `save-status`. A 30 s server timer auto-saves dirty documents; the UI also saves ~1.5 s after editing pauses and on Ctrl+S. |

`FloorView` = `{id, name, digest, grid, walls, wall_tally, lights, labels}` —
`grid` is the room grid (`spaces`, `marks`, `cells` as `{x,y,space}` triples);
`walls` are derived on the fly in **nd-world's exact wall shape**
(`{id, pts:[[x,y]…], kind: wall|door|window|secret, state?: open|closed}`).

## Editing

| Method | Path | Body | Notes |
|---|---|---|---|
| POST | `/api/docs/{id}/ops` | `{floor, ops:[…], base_rev?}` | Applies a batch as journal entries; `{"ok":true,"rev":N}`. ≤ 500 ops per request. `base_rev` is an optimistic-concurrency guard: if the document has moved past it (another window), the edit is refused with **409** `{"error","rev"}` rather than interleaved. |
| POST | `/api/docs/{id}/undo` | `{floor}` | Applies the last entry's inverse as a new entry; `{"ok":true,"rev":N}`. |
| POST | `/api/docs/{id}/floors` | `{action:"add",name,copy_from?}` / `{action:"rename",id,name}` / `{action:"remove",id}` / `{action:"move",id,index}` | Floor structure. Max 64 floors, last floor cannot be removed. Returns `{"floors":[{id,name}], "floor":id?}` (`floor` is the new id on add). |
| POST | `/api/docs/{id}/undo-structure` | – | Undoes the last floor add/rename/move/remove (a removed floor returns with its content). Returns `{"floors":[{id,name}]}`. |

| POST | `/api/docs/{id}/generate-map` | `{floor?, kind: dungeon\|cave\|building\|deck\|district, seed, width, height}` | 16–120 squares a side. Adds to the floor as one undoable step; same seed, same map. |
| GET | `/api/docs/{id}/export/pdf?floor&paper=letter\|a4&inch=1&dpi=150&audience` | – | The floor at true scale, one inch a square by default, cut into pages (≤80). White ground, corner ticks, a "page r of n" label. |
| GET | `/api/doctor` | – | The `ndcart doctor` checks as `{checks:[{level: ok\|warn\|fail, what, detail}]}` (no address check, nothing secret). |
| GET | `/api/docs/{id}/export/foundry?floor&ppc=100&image&distance=5&units=ft&audience` | – | Foundry v11/v12 scene JSON (walls, doors, windows, lights, grid). The picture is exported separately. |
| POST | `/api/import/foundry?name=` | scene JSON | New project with the scene's walls, doors and lights (no picture; add it as an underlay). |
| GET | `/api/packs/export?name=&include=all` | – | Your own props and materials as a Dungeondraft-style pack; borrowed art is left out unless `include=all`. |
| GET | `/api/world/entities?q=` | – | The world's entities (nd-world) matching the words, ≤50, for linking a room. The op `link_entity {link:{space, entity_id, name, kind}}` / `unlink_entity {space}` does the linking. |
| POST | `/api/ask` | `{question}` | Answer a question from a digest of every project's text. Needs an AI. |
| POST | `/api/packs/import?name=file` | raw `.dungeondraft_pack` bytes (≤200 MB) | Objects → props, terrain → materials; `{pack, stamps, materials, skipped, notes}`. Zip and newer archive formats are refused with advice. |
| POST / GET / DELETE | `/api/docs/{id}/table` | POST `{floor?, x, y, radius?, darkness?}` | The shared table view. GET returns what the screen polls; DELETE turns it off. The screen itself is `GET /table/{id}` (read-only HTML). |
| POST | `/api/docs/{id}/furnish` | `{floor?, spaces:[room ids], seed, only_empty?}` | Props from each room's name/kind, never in a doorway. One undo step. |
| GET | `/api/search?q=` | – | Rooms, notes, markers, zones, regions, routes, floors and projects whose text holds every word typed. ≤100 hits with where they are. |
| POST | `/api/docs/{id}/propose-world` | `{spec: WorldSpec, seed?}` | An overland map as a proposal (land/rivers, biome masks on dry ground, places, regions). World-map projects only. |
| GET | `/api/docs/{id}/world-proposals/{pid}/preview?ppc=` | – | PNG. |
| POST | `/api/docs/{id}/world-proposals/{pid}/accept` | – | Replaces the first floor's terrain/rivers; one undo step. DELETE the same path to reject. |
| POST | `/api/docs/{id}/refine` | `{floor? \| spec (a floor spec being previewed), pins:[room ids], instruction}` | AI revises the floor (described as a spec) keeping the pinned rooms; returns `{spec}` to preview as a new floor. Needs an AI configured. |
| POST | `/api/docs/{id}/style` | `{tone, palette:[#rrggbb…]}` | The project's house style (fed to drafts; the palette colours generated rooms). Empty clears. |
| POST | `/api/draft-world` | `{prompt, doc?, lore?}` | Like `/api/draft` for a WorldSpec. `lore:true` reuses the world's location names (nd-world). |
| GET | `/api/ai/status` | – | `{mode: nd-world\|standalone\|off, detail}`. Standalone = `ND_CARTO_LLM_URL` + `ND_CARTO_LLM_MODEL` (+ `ND_CARTO_LLM_KEY[_FILE]`), an OpenAI-compatible endpoint. |
| POST | `/api/ai/image` | `{kind: underlay\|material\|prop, prompt, width?, height?, count?, seed?, like?: {job, index}, strength?, upscale?}` | Queue pictures through nd-world's imagegen in the background → `{job}`. nd-world only. |
| GET | `/api/ai/jobs/{id}` | – | `{status: running\|done\|error\|cancelled, message, assets:[ids], seeds}`; POST `…/cancel`. |
| POST | `/api/assets/{id}/make-material` | `{name, size?, seamless?, prompt?, seed?, ai?}` | Make the picture seamless and add it to the palette (`mat_…`). |
| POST | `/api/assets/{id}/make-stamp` | `{name, size?, key?, tolerance?, prompt?, seed?, ai?}` | Cut the prop out of its flat background, trim it, add it to the props (`stp_…`). |
| POST | `/api/stamps/{id}/tags` | `{tags}` | The GM's confirmed tags for a custom prop; `…/suggest-tags` asks the AI (suggestions only). |
| POST | `/api/docs/{id}/underlay` | `{floor?, asset, x, y, w, h, opacity?}` | A stored picture behind the floor, one undo step. |
| POST | `/api/docs/{id}/propose` | `{spec: MapSpec, seed?}` | Lay a room-graph spec out and hold it as a proposal (nothing changes). 400 with a fix-it message if the spec breaks a rule. |
| GET | `/api/docs/{id}/proposals/{pid}/preview?floor=N&ppc=` | – | PNG of one proposed floor. |
| POST | `/api/docs/{id}/proposals/{pid}/accept` | – | Adds the floors (and stairs) as new floors; undo with `undo-structure` once per floor. |
| DELETE | `/api/docs/{id}/proposals/{pid}` | – | Reject. |
| POST | `/api/draft` | `{prompt}` | Ask nd-world's AI for a MapSpec. Needs `ND_CARTO_SERVER`, `ND_CARTO_EMAIL`, `ND_CARTO_PASSWORD[_FILE]` in the studio's environment (404 naming what is missing otherwise). |
| GET | `/api/docs/{id}/preview?floor&x&y&radius&darkness&ppc` | – | PNG of what the table sees from a point (players' copy, fog, sight blocked by walls and closed doors, lamps light what they reach). |
| POST | `/api/docs/{id}/scatter` | `{floor?, points:[[x,y]…], biome? \| assets[], spacing?, density?, seed}` | Fill an area with props (and, for a biome, a tinted region) as one undoable step. Biomes: forest, meadow, rocky, swamp, desert. |
| GET | `/api/prefabs` | – | The prefab library shared by every project. |
| POST | `/api/docs/{id}/prefabs` | `{floor?, name, spaces:[room ids]}` | Copy rooms (with doors, lights, notes on them) into the library. |
| DELETE | `/api/prefabs/{pid}` | – | |
| POST | `/api/docs/{id}/place-prefab` | `{floor?, prefab, x, y, turns?}` | One undo step; fresh room ids; `turns` = quarter turns clockwise. |
| GET / POST | `/api/docs/{id}/checkpoints` | POST `{name}` | Named save points of every floor (max 30). |
| DELETE | `/api/docs/{id}/checkpoints/{cid}` | – | |
| POST | `/api/docs/{id}/checkpoints/{cid}/restore` | – | Replaces all floors; the replaced state becomes a new "Before restoring…" checkpoint; undo history restarts. |

Floors carry an optional `map_grid {kind: square|hex, size (cells, 0.5-200), color, opacity}` set with the op `set_map_grid` (null removes it).

Tiles accept `?onion=<floor id>`: that floor is drawn at 30% opacity under the current floor (404 if the id is unknown).

## Inspection, history and live sync

| Method | Path | Body | Notes |
|---|---|---|---|
| GET | `/api/docs/{id}/lint?floor=ID` | – | The Problems panel: deterministic checks, each with an undoable `fix` op you can POST to `/ops`. |
| GET | `/api/docs/{id}/stats?floor=ID` | – | The pacing dashboard: rooms/squares/kinds, doors and secrets, light coverage, props by provenance, GM labels, encounters, lore coverage. |
| GET | `/api/docs/{id}/floor-spec?floor=ID` | – | Read a painted floor back as a MapSpec (the spec round-trip) to lay out again with a new seed. Changes nothing. |
| GET | `/api/docs/{id}/history?floor=ID` | – | The journal summarized one line per entry (oldest first) with its `index`, for the history browser. |
| POST | `/api/docs/{id}/revert` | `{floor, index}` | Jump to the state at `index`: the difference is applied as ONE undoable batch (nothing is lost; undo returns to now). Busts the tile cache. |
| GET | `/api/docs/{id}/events` | – | Server-sent events of the document's revision: `rev` events on change, `gone` if the project is deleted. The watch-mode / table-screen sync feed. |

## More exports

| Method | Path | Notes |
|---|---|---|
| GET | `/api/docs/{id}/export.svg?floor=ID&audience=gm\|player` | The floor's geometry as editable SVG (rooms as rects, walls/doors as lines, labels as text; 1 cell = 1 user unit). |
| GET | `/api/docs/{id}/export/roll20?floor=ID` | A Roll20 wall-data sidecar (polylines at 70 px/square). **Unverified live** — Roll20 wall import scripts vary. |

## AI settings and world tables

| Method | Path | Body | Notes |
|---|---|---|---|
| GET / POST | `/api/ai-settings` | POST `{direct_url?, direct_model?, key?}` | The saved standalone-model config (Unsloth / Ollama / any OpenAI-compatible endpoint). GET is redacted (the key is only "set"/"not set"). |
| GET | `/api/ai-models` | – | The endpoint's own model list, for the settings dropdown. |
| POST | `/api/ai-test` | – | One tiny request to the configured model to prove the connection. |
| POST | `/api/docs/{id}/theme-pack` | `{prompt}` | Draft a theme pack's material/prop prompts as JSON (the picture generation is a separate step). |
| GET | `/api/world/tables` | – | The world's random tables (nd-world), for stocking rooms. |
| POST | `/api/docs/{id}/stock-room` | `{floor?, room, table_name}` | Roll one weighted pick from a named world table into the room's encounter (GM-only; one undo step). Needs nd-world. |
| GET | `/api/world/entities/{id}/preview` | – | The linked entity's lore as nd-world shows the GM (the lore-sync read half), plus the server URL to deep-link. |
| POST | `/api/docs/{id}/reroll` | `{floor?, room, seed?}` | Replace exactly one room's generated props; pinned and hand-placed props stay. One undo step. |
| POST | `/api/assets/{id}/meta` | v2 metadata | Set an asset's footprint, clearance, placement, style tags and licence. The picture is untouched. |

### Ops (internally tagged, `"t"`)

- `{"t":"paint_cells","cells":[{"x","y"}…],"space":"id"|null}` — paint or clear
- `{"t":"upsert_space","space":{"id","name","color":"#rrggbb","kind":"room|corridor|area","material?":"stone_floor"}}` — `material` is an id from `GET /api/materials` (1–40 chars `a-z0-9_`); without one the flat `color` paints
- `{"t":"add_stroke","stroke":{"id","kind":"paint|erase","brush":"round|scatter","material?","color":"#rrggbb","size":0.05-64,"opacity":0-1,"hardness?":0-1,"roughness?":0-1,"density?":0-1,"seed","points":[{"x","y","pressure?":0-1,"tilt?":0-1}]},"index?"}` — a paint stroke in cell coordinates (1–4,000 points, ≤ 20,000 per floor); `index` is its z-order position (default: on top)
- `{"t":"remove_stroke","id"}`
- `{"t":"add_stamp","stamp":{"id","asset":"crate","x","y","scale?":0.1-8,"rotation?","flip?","tint?":"#rrggbb","shadow?"},"index?"}`, `{"t":"update_stamp","stamp":{…same id…}}`, `{"t":"remove_stamp","id"}` — placed props (`GET /api/stamps` lists the pack)
- `{"t":"set_underlay","underlay":{"asset":"<sha256>","x","y","w","h","opacity?"}|null}` — the picture behind everything (asset ids come from `POST /api/assets`); `{"t":"set_imported_walls","walls":[{"pts":[[x,y],…],"kind","state?"}]}` — walls not derived from rooms, in cells
- Region maps: `{"t":"terrain_stroke","stroke":{"mode":"raise|lower|smooth|flatten|roughen","points":[[x,y],…],"radius":0.2-64,"strength":0-1,"seed?"}}`, `{"t":"terrain_set","fill","chunks":[{"cx","cy","data":"<base64 64x64 u16 LE>"}]}`, `{"t":"set_terrain_style","style":{"sea_level?","tint?","tint_amount?","saturation?","contrast?","brightness?","hillshade?","contour_interval?"}}`, `{"t":"add_river","river":{"id","name","points":[[x,y],…],"width":0.05-16},"carve?":true,"index?"}`, `{"t":"remove_river","id"}`, `{"t":"clear_terrain"}`, `{"t":"set_canvas","canvas":[cols,rows]|null}`
- `{"t":"put_overlay","item":{"kind":"marker","id","x","y","label","note?","color?","link_schematic?"}|{"kind":"region","id","name","points":[[x,y],…],"color?","opacity?","hatch?"}|{"kind":"route","id","name","points":[[x,y],…],"color?","width?","dashed?"},"index?"}` (same kind+id replaces in place), `{"t":"remove_overlay","kind":"marker|region|route","id"}` — overlay items in cells (≤ 500 per kind)
- `{"t":"upsert_label","label":{"id","x","y","text","size":0.1-40,"color":"#rrggbb","bold","gm_only?","path?":[[x,y],…],"halo?","tracking?":0-1,"rotation?"}}` / `{"t":"remove_label","id"}` — map text in cells; with `path` the text follows that polyline
- `{"t":"remove_space","id"}`
- `{"t":"set_mark","edge":{"cell":{"x","y"},"edge":"west|north|east|south"},"mark":{"kind":"opening|door|window|secret","state":"open|closed"}|null}`
- `{"t":"add_light","light":{"id","x","y","range","color","intensity","on","label?"}}` — ≤ 60 per floor (nd-world publish cap)
- `{"t":"remove_light","id"}`
- `{"t":"upsert_label","label":{"id","x","y","text","size","color","bold"}}`
- `{"t":"remove_label","id"}`
- `{"t":"batch","ops":[…]}` — one undoable step

Validation is at the boundary (the D1 lesson): every number finite and bounded,
referenced ids must exist, caps enforced. Refused ops are never journaled.
Status codes: 400 invalid/out-of-bounds, 404 not found, 409 stale `base_rev`,
422 cap exceeded.

## Materials

| Method | Path | Notes |
|---|---|---|
| GET | `/api/materials` | The starter pack: `{pack, materials:[{id, name, color}]}`; `color` is the base colour for swatches. |

## Generate a world

| Method | Path | Notes |
|---|---|---|
| POST | `/api/docs/{id}/generate` | Body `{seed?, cols?, rows?, land?, continent?, ridges?, rivers?}` (size capped at 512×512 cells). Replaces the first floor's terrain and rivers as one undoable step; returns `{cols, rows, rivers, sea_level, chunks}`. Same seed ⇒ the same world. |

## Assets and import

| Method | Path | Notes |
|---|---|---|
| POST | `/api/assets` | Raw image bytes (≤ 80 MB, ≤ 40 Mpx, PNG/JPEG/GIF/BMP/WebP) → `{id}` = the SHA-256. Anything that does not decode → 400. |
| GET | `/api/assets/{id}` | The stored picture (immutable, cacheable). 404 for unknown or malformed ids. |
| POST | `/api/import/uvtt[?name=]` | A Universal VTT file in the body → a new project (`{manifest, floor, stats}`): picture → underlay, walls/portals/windows → imported walls, lights → lights, grid = the file's squares. Bad file → 400 and no project left behind. |

## Stamps

| Method | Path | Notes |
|---|---|---|
| GET | `/api/stamps` | The starter pack: `{pack, stamps:[{id, name, size}]}` (size in cells). |
| GET | `/api/stamps/{id}/icon.png` | A 64 px icon for palettes. |

## Tiles

| Method | Path | Notes |
|---|---|---|
| GET | `/tiles/{doc}/{floor}/{z}/{x}_{y}?rev=N` | 256px PNG. Zoom pyramid: z0 = 16 px/cell, doubles per level, z ≤ 4. `rev` busts caches after edits. Coordinates are bounded (±2²⁰ tiles) so a wild value cannot overflow the baker → 400. |

## Export

| Method | Path | Notes |
|---|---|---|
| GET | `/api/docs/{id}/export/uvtt[?floor=ID&ppc=N&format=png\|webp]` | Universal VTT 0.3 JSON: walls, door portals, lights and the embedded full-map picture (default 32 px/cell, PNG). `audience=player` strips secrets (see below). `format=avif` is refused: VTTs and nd-world's `import-uvtt` don't read it. |
| GET | `/api/docs/{id}/export/png[?floor=ID&ppc=N&format=png\|webp\|avif&quality=1-100]` | The whole floor as one image. Despite the path it serves `image/png`, `image/webp` (lossless) or `image/avif` (lossy, `quality` default 80). Limits: ppc 1..=256, each side ≤ 20,000 px, area ≤ 40 Mpx (nd-world's import caps) → 400 otherwise. |

`audience=gm|player` (default `gm`) on both exports: `player` runs the floor through the
one secrets filter — secret doors become plain walls, GM-only labels are dropped.

## Convert

| Method | Path | Notes |
|---|---|---|
| POST | `/api/convert?format=webp\|avif\|png[&quality=1-100]` | Raw image bytes in (PNG/JPEG/GIF/BMP/WebP, ≤ 80 MB, ≤ 40 Mpx, ≤ 20,000 px a side); converted image out with `x-image-width` / `x-image-height`. Default `webp` (lossless). AVIF input → 400. |

## Access control

Listening off loopback requires a token: `Authorization: Bearer T`, cookie
`ndcart_token=T`, or `?token=T` (sets the cookie). `/health` is public. Token from
`ND_CARTO_TOKEN`, `ND_CARTO_TOKEN_FILE`, or `<data-dir>/token` (generated 0600 on
first run, printed once). 401 without it. The token is drawn from the OS CSPRNG
on every platform (Unix and Windows).

On loopback there is no token, but a `Host` header that is not a loopback name
(`localhost`, `127.0.0.1`, `[::1]`, any port) is refused with 403 — the
DNS-rebinding guard, so a malicious web page cannot point its own domain at
127.0.0.1 and read/write the studio same-origin. `/health` stays open.

The shell files (`/`, `/app.js`, `/app.css`) are served with `Cache-Control:
no-cache` and an ETag (a matching `If-None-Match` gets a 304), so a browser always
revalidates after a server upgrade instead of running stale JS against a new API.

## CLI

```
ndcart serve [--listen 127.0.0.1:8300] [--data-dir DIR]   # env: ND_CARTO_LISTEN, ND_CARTO_DATA_DIR
ndcart bench [--ppc N]                                    # journal/walls/replay + full-map bake timings (asserts the 30 s budget)
ndcart convert IN OUT [--format webp|avif|png] [--quality N]
ndcart publish DOC --server URL --email E [--world W] [--floor ID | --all-floors] [--all-docs] [--report FILE] [--keep-going] [--publish-anyway] [--ppc 32] [--format webp] [--max-mb 20] [--audience gm|player] [--keep-overlay] --data-dir DIR
                                                          # password: ND_CARTO_PASSWORD / ND_CARTO_PASSWORD_FILE only
```

With `--data-dir`, each document persists as `<dir>/<doc-id>/{manifest.json,
snapshot.json, journal.ndjson, structure.ndjson}` and is reloaded at startup.
Edits append to the journal (fsync throttled); the snapshot is rewritten every
few hundred edits or minute, and a reload replays the journal's tail over it — so
a crash loses nothing that was acknowledged. The journal compacts at a cap
(checkpoints are the long-term history).
