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
