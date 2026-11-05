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

### Ops (internally tagged, `"t"`)

- `{"t":"paint_cells","cells":[{"x","y"}…],"space":"id"|null}` — paint or clear
- `{"t":"upsert_space","space":{"id","name","color":"#rrggbb","kind":"room|corridor|area"}}`
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

## Tiles

| Method | Path | Notes |
|---|---|---|
| GET | `/tiles/{doc}/{floor}/{z}/{x}_{y}?rev=N` | 256px PNG. Zoom pyramid: z0 = 16 px/cell, doubles per level, z ≤ 4. `rev` busts caches after edits. |

## Export

| Method | Path | Notes |
|---|---|---|
| GET | `/api/docs/{id}/export/uvtt[?floor=ID&ppc=N&format=png\|webp]` | Universal VTT 0.3 JSON: walls, door portals, lights and the embedded full-map picture (default 32 px/cell, PNG). `format=avif` is refused: VTTs and nd-world's `import-uvtt` don't read it. |
| GET | `/api/docs/{id}/export/png[?floor=ID&ppc=N&format=png\|webp\|avif&quality=1-100]` | The whole floor as one image. Despite the path it serves `image/png`, `image/webp` (lossless) or `image/avif` (lossy, `quality` default 80). Limits: ppc 1..=256, each side ≤ 20,000 px, area ≤ 40 Mpx (nd-world's import caps) → 400 otherwise. |

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
ndcart bench                                              # journal/walls/replay timings
ndcart convert IN OUT [--format webp|avif|png] [--quality N]
ndcart publish DOC --server URL --email E [--world W] [--floor ID] [--ppc 32] [--format webp] [--max-mb 20] --data-dir DIR
                                                          # password: ND_CARTO_PASSWORD / ND_CARTO_PASSWORD_FILE only
```

With `--data-dir`, each document persists as `<dir>/<doc-id>/{manifest.json,
floors.json, journal.ndjson}` and is reloaded at startup (journal replay
rebuilds the state; M0 rewrites the files whole on save — append-only segments
arrive with the `.ndmap` work).
