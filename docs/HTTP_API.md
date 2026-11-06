# nd-cartographer HTTP API — M0

The studio's own HTTP surface is a product contract (the desktop shell, browser
mode and the CLI all sit on it; a future MCP surface wraps the same calls — plan
§17.6). This file documents what exists in M0 and evolves with the code.

Everything is JSON unless noted. No auth in M0 (loopback; the token gate for
non-loopback listeners is specified in plan §11.2 and lands before any LAN use).

## Health

| Method | Path | Notes |
|---|---|---|
| GET | `/health` | `{"status":"ok","app":"nd-cartographer","version":"…"}` — public |

## Documents

| Method | Path | Body | Notes |
|---|---|---|---|
| GET | `/api/docs` | – | `{"docs":[{id,name,kind,rev,floors:[names]}]}` |
| POST | `/api/docs` | `{name, kind?}` | kind: `battle` (default) / `region` / `schematic`. Returns the `Manifest`. Creates floor "Ground". |
| GET | `/api/docs/{id}` | – | `{"manifest", "rev", "floors":[FloorView]}` |

`FloorView` = `{id, name, digest, grid, walls, wall_tally, lights, labels}` —
`grid` is the room grid (`spaces`, `marks`, `cells` as `{x,y,space}` triples);
`walls` are derived on the fly in **nd-world's exact wall shape**
(`{id, pts:[[x,y]…], kind: wall|door|window|secret, state?: open|closed}`).

## Editing

| Method | Path | Body | Notes |
|---|---|---|---|
| POST | `/api/docs/{id}/ops` | `{floor, ops:[…]}` | Applies a batch as journal entries; `{"ok":true,"rev":N}`. ≤ 500 ops per request. |
| POST | `/api/docs/{id}/undo` | `{floor}` | Applies the last entry's inverse as a new entry; `{"ok":true,"rev":N}`. |
| POST | `/api/docs/{id}/floors` | `{action:"add",name,copy_from?}` / `{action:"rename",floor,name}` / `{action:"remove",floor}` / `{action:"move",floor,to}` | Floor structure. Max 64 floors, last floor cannot be removed. `{"ok":true,"rev":N,"floor":id?}`. |
| POST | `/api/docs/{id}/undo-structure` | – | Undoes the last floor add/rename/move/remove (a removed floor returns with its content). |

| POST | `/api/docs/{id}/generate-map` | `{floor?, kind: dungeon\|cave\|building\|deck\|district, seed, width, height}` | 16–120 squares a side. Adds to the floor as one undoable step; same seed, same map. |
| GET | `/api/docs/{id}/export/pdf?floor&paper=letter\|a4&inch=1&dpi=150&audience` | – | The floor at true scale, one inch a square by default, cut into pages (≤80). White ground, corner ticks, a "page r of n" label. |
| GET | `/api/doctor` | – | The `ndcart doctor` checks as `{checks:[{level: ok\|warn\|fail, what, detail}]}` (no address check, nothing secret). |
| GET | `/api/docs/{id}/export/foundry?floor&ppc=100&image&distance=5&units=ft&audience` | – | Foundry v11/v12 scene JSON (walls, doors, windows, lights, grid). The picture is exported separately. |
| POST | `/api/import/foundry?name=` | scene JSON | New project with the scene's walls, doors and lights (no picture; add it as an underlay). |
| GET | `/api/packs/export?name=&include=all` | – | Your own props and materials as a Dungeondraft-style pack; borrowed art is left out unless `include=all`. |
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
| POST | `/api/ai/image` | `{kind: underlay\|material\|prop, prompt, width?, height?, count?, seed?}` | Queue pictures through nd-world's imagegen in the background → `{job}`. nd-world only. |
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
Status codes: 400 invalid/out-of-bounds, 404 not found, 422 cap exceeded.

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
| GET | `/tiles/{doc}/{floor}/{z}/{x}_{y}?rev=N` | 256px PNG. Zoom pyramid: z0 = 16 px/cell, doubles per level, z ≤ 4. `rev` busts caches after edits. |

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
first run, printed once). 401 without it.

## CLI

```
ndcart serve [--listen 127.0.0.1:8300] [--data-dir DIR]   # env: ND_CARTO_LISTEN, ND_CARTO_DATA_DIR
ndcart bench [--ppc N]                                    # journal/walls/replay + full-map bake timings (asserts the 30 s budget)
ndcart convert IN OUT [--format webp|avif|png] [--quality N]
ndcart publish DOC --server URL --email E [--world W] [--floor ID | --all-floors] [--all-docs] [--report FILE] [--keep-going] [--publish-anyway] [--ppc 32] [--format webp] [--max-mb 20] [--audience gm|player] [--keep-overlay] --data-dir DIR
                                                          # password: ND_CARTO_PASSWORD / ND_CARTO_PASSWORD_FILE only
```

With `--data-dir`, each document persists as `<dir>/<doc-id>/{manifest.json,
floors.json, journal.ndjson}` and is reloaded at startup (journal replay
rebuilds the state; M0 rewrites the files whole on save — append-only segments
arrive with the `.ndmap` work).
