# nd-cartographer

A map studio for tabletop games, written in Rust and run in your browser: paint
battle maps, generate dungeons and whole worlds, print them at true scale, and
(optionally) publish them to [nd-world](https://github.com/8bit-boom/nd-world)
with doors, lights and walls ready to play. It works fully on its own; nothing
requires nd-world or an AI.

## Quick start

```bash
cargo run --release -p ndcart-server --bin ndcart -- serve --data-dir ~/ndcart-data
```

Then open http://127.0.0.1:8300. Your projects live in the data folder.
If something does not work, run `ndcart doctor` (it checks the folder, the
picture formats, the font, the port, and the AI/nd-world settings, and tells
you what to fix). A Docker image (`Dockerfile`) serves the same studio on port
8300 with `/data` as the volume; [docs/DOCKER.md](docs/DOCKER.md) has the TrueNAS SCALE steps and a compose file.

## What you can do

- **Paint battle maps**: rooms, corridors, doors, windows, secret doors; material
  painting with edge blending, freehand brushes, props with shadows, lights,
  text (with text along a path), floors with stairs between them, onion skin.
- **Generate**: dungeons, caves, buildings, ship decks, city districts, and
  overland worlds (continents, mountains, rivers, forests, swamps). Seeded: the
  same seed always gives the same map, and every generation is one undo step.
- **Work at scale**: zones, multi-select and bulk edits, symmetric painting,
  prefabs you can reuse across projects, named checkpoints, a minimap, saved
  views, search across all your maps, a problems panel with one-click fixes.
- **Player view**: see exactly what the table sees from a spot (line of sight,
  closed doors, lamps), and show it on a TV or tablet at `/table/<project>`.
- **Print**: a PDF at one inch per square, cut into pages with corner ticks.
- **Export**: PNG, WebP, AVIF, Universal VTT, Foundry VTT scene data, players'
  copy without secrets; import Universal VTT and Foundry scenes.
- **Bring your own art**: import a Dungeondraft pack or a folder of pictures;
  turn any picture into a seamless material or a cut-out prop. See
  [docs/PACK.md](docs/PACK.md).
- **Publish to nd-world** (optional): one map or every project and floor at
  once, updating in place on later runs.
- **AI help** (optional): describe a map in words and get a preview to accept or
  reject; revise a floor while keeping the rooms you pin; pictures through
  nd-world's image queue. See [docs/AI_CARTOGRAPHER_GUIDE.md](docs/AI_CARTOGRAPHER_GUIDE.md)
  and [docs/MAP_SPEC.md](docs/MAP_SPEC.md).

## Commands

| Command | What it does |
|---|---|
| `ndcart serve [--listen ADDR] [--data-dir DIR]` | the studio (default `127.0.0.1:8300`; on a non-local address it asks for a token) |
| `ndcart doctor` | check the setup |
| `ndcart import-pack FILE_OR_FOLDER --data-dir DIR` | add a Dungeondraft pack or a folder of art |
| `ndcart open-published SLUG --server URL --email E --data-dir DIR` | bring a published map back as a project (picture, walls, lights) |
| `ndcart publish DOC --server URL --email E [--all-floors] [--all-docs] [--report FILE]` | publish to nd-world |
| `ndcart draft "a flooded crypt" ...` | ask an AI for a map spec |
| `ndcart convert IN OUT` | convert pictures between PNG, WebP and AVIF |
| `ndcart bench` | time the renderer |

Passwords and keys are read from the environment (`ND_CARTO_PASSWORD` or
`ND_CARTO_PASSWORD_FILE`, `ND_CARTO_LLM_KEY[_FILE]`), never from arguments, and
are never printed or logged. The HTTP API is in [docs/HTTP_API.md](docs/HTTP_API.md);
how it is built is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## What is not here yet

Honest list, so nothing surprises you: a desktop shell exists (`ndcart-desktop`, a
Tauri v2 window over the same server — [docs/DESKTOP.md](docs/DESKTOP.md)) but no
signed installers yet and it has never run outside CI; no diagram/schematic mode; Foundry scenes are exported and imported
but not tried in a running Foundry; the Dungeondraft pack reader follows the Godot 3
archive format but has not been tried on many real packs; Windows and macOS builds
are set up (a release workflow) but have never been run; and the AI features have
been exercised against stand-ins for nd-world and a model, not a real model's
output. The plan and what is done are tracked in
[docs/ND_CARTOGRAPHER_PLAN.md](docs/ND_CARTOGRAPHER_PLAN.md) §20.

## Layout

| Crate | Role |
|---|---|
| `ndcart-core` | document model, command journal, room grid, walls, generators, specs, linter |
| `ndcart-paint` | materials and blending, brushes, the built-in props |
| `ndcart-terrain` | heightmaps, shading, rivers, world generator |
| `ndcart-formats` | Universal VTT |
| `ndcart-bridge` | the optional nd-world client and direct-model client |
| `ndcart-server` | the studio: web UI, API, tile baker, exports |
| `ndcart-desktop` | the Tauri v2 desktop shell (a window over the same server) |

## License

MIT, see [LICENSE](LICENSE). Imported art packs stay under their own licenses;
the studio never redistributes them, and every imported item keeps the name of
the pack and its author.
