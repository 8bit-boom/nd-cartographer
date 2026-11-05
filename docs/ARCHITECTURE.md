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
