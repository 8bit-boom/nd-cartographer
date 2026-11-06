# Map specs

A spec describes a map as **meaning**, not pixels. The studio lays it out with its
own deterministic code, so a spec can be previewed, re-rolled with another seed,
and is validated before anything is drawn. An AI or a person writes the spec;
nothing is added to a project until it is accepted.

Two kinds: a **MapSpec** (rooms and how they connect) and a **WorldSpec** (an
overland map). Paste either into the studio's **✨ From a spec…** dialog, or let
**✨ Draft with AI** write it.

## MapSpec (battle maps)

```json
{
  "name": "Abandoned metro station",
  "floors": [
    {
      "name": "Platform",
      "rooms": [
        {"id": "hall",   "name": "Ticket Hall",    "w": 9,  "h": 6},
        {"id": "plat",   "name": "Platform",       "w": 14, "h": 4},
        {"id": "office", "name": "Control Office", "w": 5,  "h": 5, "lit": false},
        {"id": "store",  "name": "Hidden Store",   "w": 4,  "h": 4}
      ],
      "links": [
        {"a": "hall",   "b": "plat",  "kind": "opening"},
        {"a": "hall",   "b": "office"},
        {"a": "office", "b": "store", "kind": "secret"}
      ],
      "stairs": [{"room": "plat", "to_floor": "Tunnels", "to_room": "tun", "label": "Service stairs"}]
    },
    {
      "name": "Tunnels",
      "rooms": [{"id": "tun", "name": "Service Tunnel", "w": 12, "h": 3}],
      "links": []
    }
  ]
}
```

| Field | Rule |
|---|---|
| `name` | 1-80 characters |
| `floors` | 1-12; names unique, 1-40 characters |
| `rooms` | 1-60 per floor; `id` unique, 1-40 of `a-z 0-9 _ -`; `name` 1-60; `w`,`h` 2-30 squares; `kind` `room` (default), `corridor` or `area`; `lit` default true (a lamp in the room) |
| `links` | `a`,`b` are room ids on the same floor; `kind` `door` (default), `opening` (an archway) or `secret` |
| `stairs` | `room` on this floor; `to_floor` another floor's name; optional `to_room` and `label`. The way back is added unless the other floor already has stairs here |

**Every room must be reachable from the first room through links.** A spec that
breaks a rule is refused with a message naming what to fix (the same message is
sent back to an AI so it can correct itself). Sizes are in squares; positions are
never given.

Accepting adds **new floors** to the project; existing floors are never touched.
Each added floor is one step of "Undo floors".

## WorldSpec (overland maps)

Coordinates are fractions of the map (0 to 1, x to the right, y downward).

```json
{
  "name": "The Cold Archipelago",
  "cols": 120, "rows": 80,
  "land": 0.38, "continent": 0.25, "ridges": 0.7, "rivers": 6,
  "biomes": [
    {"biome": "forest", "x": 0.5, "y": 0.5, "rx": 0.2, "ry": 0.15},
    {"biome": "rocky",  "x": 0.3, "y": 0.35, "rx": 0.12, "ry": 0.1, "density": 1.5}
  ],
  "places":  [{"name": "Port Neon", "note": "A smugglers' harbour.", "x": 0.5, "y": 0.5}],
  "regions": [{"name": "The Wild North", "points": [[0.15,0.1],[0.85,0.1],[0.85,0.35],[0.15,0.35]]}]
}
```

| Field | Rule |
|---|---|
| `cols`,`rows` | 16-512 (default 160 × 110) |
| `land` | 0.05-0.95, the fraction that is dry |
| `continent` | 0 (scattered islands) to 1 (one central continent) |
| `ridges` | 0-1, mountain chains; `rivers` 0-40 |
| `biomes` | ellipses (`x`,`y` centre; `rx`,`ry` 0.02-0.5); `biome` is `forest`, `meadow`, `rocky`, `swamp` or `desert`; `density` 0.5-4 (higher is sparser). Props land only on dry ground |
| `places` | markers with a name and note; one that falls in the sea is moved to the nearest land, or refused if none is near |
| `regions` | named polygons of 3-100 points, `color` optional |

Accepting a world **replaces the project's terrain and rivers** (one undo step) and
adds the props, places and regions. Only world-map projects accept one.

## Seeds

The preview uses a random seed; **Another layout** tries a new one. The same
spec and seed always give the same map, on every machine.
