# Playing maps in nd-world

nd-cartographer works on its own, but it plugs into
[nd-world](https://github.com/8bit-boom/nd-world) (the Rust-only gateway,
`ND_PYTHON_FREE=1`) for two things: **installing the studio from inside nd-world**,
and **playing a published map in one click**.

## Install / uninstall from nd-world

nd-world's **Settings → AI Tools → Map Maker** (`/cartographer`) manages the studio
as a child process — no separate install:

- **Install** downloads the latest release binary for the platform (the Linux
  release is musl-static, so it runs inside nd-world's container), starts it on a
  free local port with its own token, and embeds it in an iframe. A prepared
  binary can be supplied with `ND_CARTOGRAPHER_BIN_DIR` (tests / air-gapped).
- **Start / Stop** control the running studio; **Uninstall** stops it and removes
  the nav entry. Your maps live in `{data}/cartographer/data` and are kept.
- The studio autostarts with nd-world when left enabled.

The studio side needs no change: `ndcart serve --listen … --data-dir …` with
`ND_CARTO_TOKEN` is exactly what the desktop shell already does.

## Publish, then play

1. In the studio, give rooms **encounters** (the ⚔ button): creature lines
   (`goblin x2, ogre`) and **loot** (`gold x50, potion x2`). A treasure-only room
   is allowed.
2. **Publish** the map to nd-world (`ndcart publish …`). Alongside the walls,
   lights, rooms and picture, the studio sends:
   - one **combat template** per encounter room (`POST /api/combat/templates`), and
   - a **play manifest** (`POST /api/maps/schematic/{slug}/play-manifest`): per room,
     its NPCs and loot.
3. In nd-world, open the map under **Maps** and press **▶ Play**
   (`POST /api/maps/schematic/{slug}/play`). nd-world builds a new combat from the
   party's PCs (or a chosen party) and the rooms' NPCs — resolving a creature name
   to a world entity where one exists, else a manual combatant — links it to the
   schematic, and places tokens: PCs clustered at the centre, NPCs in their rooms,
   and an item token per loot entry. It opens the combat tracker and the player
   live view.

The play manifest is stored beside the data folder (`play-manifests/{slug}.json`);
no schema change is needed. Encounters and loot are GM information and never ride
the player-visible publish.

## Routes

| Route (nd-world) | Purpose |
|---|---|
| `GET /cartographer`, `GET /api/cartographer/status` | management page + status |
| `POST /cartographer/install\|uninstall\|start\|stop` | lifecycle (GM-only) |
| `POST /api/combat/templates` | encounter template from creature lines |
| `POST /api/maps/schematic/{slug}/play-manifest` | store per-room NPCs + loot |
| `POST /api/maps/schematic/{slug}/play` | one-click scene assembly |

Standalone parity holds: with no nd-world configured, none of this runs and the
studio is a complete creator on its own.