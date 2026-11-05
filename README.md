# nd-cartographer

A standalone TTRPG map creator & editor in Rust — battle-map painting in the
Dungeondraft class, world/region map-making in the Canvas of Kings class, big-and-
complex made easy, AI as a co-cartographer — with deep, optional integration into
[nd-world](https://github.com/8bit-boom/nd-world).

**Status: the founding plan is in [docs/ND_CARTOGRAPHER_PLAN.md](docs/ND_CARTOGRAPHER_PLAN.md);
implementation (M0) is starting.** Nothing here works yet.

## What it will be

- **Standalone first** — fully usable offline, for any TTRPG system and any table
  (nd-world, Foundry, Roll20, paper). No feature requires the server.
- **Battle maps** — material painting with automatic edge blending, pattern brushes,
  pen pressure/tilt, stamps with shadows and recoloring, painted light, caves;
  print-quality export.
- **World maps** — heightmap terrain (hillshade, hypso tints, carving rivers, biomes),
  mass scatter as density fields, typography with text-along-path, political regions.
- **Generators** — seeded, deterministic dungeons, caves, buildings, city districts,
  overland terrain; every room reachable, links never guessed.
- **AI co-cartographer** — the model plans *semantic* specs (room kinds, links,
  biome mixes), deterministic Rust code builds the geometry; and it makes the art
  itself (seamless materials, cut-out stamps, mini-packs). Every AI output is a
  reviewable, undoable proposal.
- **nd-world integration (optional)** — one press publishes a *playable* map
  (background, grid, walls, doors, lights, fog-ready) via nd-world's verified HTTP
  API, plus a table remote (doors/lights/fog) and TV-mode deep links.
- **Interchange** — Universal VTT in/out, Foundry scenes, Dungeondraft/Wonderdraft
  pack import, PNG/WebP/PDF out.

## Shape (one binary, three ways to run)

| Crate | Role |
|---|---|
| `ndcart-core` | document model, command journal, room grid, walls-from-rooms, generators |
| `ndcart-paint` | brush engine, materials + blending, patterns |
| `ndcart-terrain` | heightmaps, hillshade, rivers, biomes |
| `ndcart-formats` | UVTT, Foundry, art-pack interchange |
| `ndcart-render` | tile baker (tiny-skia + rayon), LOD pyramid |
| `ndcart-server` | axum app: UI, REST + WebSocket, tiles, health |
| `ndcart-ai` | MapSpec proposals, LLM/imagegen clients |
| `ndcart-bridge` | the optional nd-world client (publish, table remote) |
| `ndcart-desktop` | Tauri v2 shell (Windows/Linux) |

`ndcart` desktop · `ndcart serve` (Docker/browser) · `ndcart gen|export|publish` (CLI).
UI: TypeScript + WebGL2/Canvas2D, all dependencies vendored with licenses.

## Status

See the plan's milestone table: **M0 — Skeleton** is in progress
(workspace, CI, document model + journal, tile baker, viewport, Tauri/Docker shells,
`ndcart bench`).

## License

MIT — see [LICENSE](LICENSE). First-party art packs will be MIT too; the studio never
redistributes third-party art-pack contents (each pack stays under its own license,
surfaced at import).
