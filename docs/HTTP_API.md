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
ndcart publish DOC --server URL --email E [--world W] [--floor ID] [--ppc 32] [--format webp] [--max-mb 20] [--audience gm|player] [--keep-overlay] --data-dir DIR
                                                          # password: ND_CARTO_PASSWORD / ND_CARTO_PASSWORD_FILE only
```

With `--data-dir`, each document persists as `<dir>/<doc-id>/{manifest.json,
floors.json, journal.ndjson}` and is reloaded at startup (journal replay
rebuilds the state; M0 rewrites the files whole on save — append-only segments
arrive with the `.ndmap` work).
