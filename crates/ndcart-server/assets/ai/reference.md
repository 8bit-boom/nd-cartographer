# nd-cartographer spec reference

pack {{PACK_VERSION}} · schema {{SCHEMA_VERSION}} · ndcart {{CARGO_PKG_VERSION}}

## 1. Your job

You write a **spec**: what a map means, never pixels or coordinates. The studio lays it out with
its own deterministic code, then the game master previews the result and decides whether to
accept it. Nothing you write changes a project until they do.

Reply with **one JSON object** and nothing else: no prose, no code fences, no comments.

## 2. MapSpec (a battle map)

| Where | Field | Rule |
|---|---|---|
| map | `name` | 1-80 characters |
| map | `floors` | 1-{{MAX_FLOORS}} floors |
| floor | `name` | unique on the map, 1-40 characters |
| floor | `rooms` | 1-{{MAX_ROOMS}} rooms; the first room is the entrance |
| floor | `links` | how rooms connect (see below); optional |
| floor | `stairs` | links to ANOTHER floor; optional |
| room | `id` | unique on its floor, 1-40 characters of `A-Z a-z 0-9 _ -`; lowercase is best |
| room | `name` | 1-60 characters; evocative, and it picks the props (section 4) |
| room | `w`, `h` | size in squares, {{ROOM_MIN}}-{{ROOM_MAX}} each way |
| room | `kind` | `room` (default), `corridor` or `area` (open ground such as a cave or square) |
| room | `lit` | `true` (default) puts a lamp in the room |
| link | `a`, `b` | two room ids on the same floor; a room cannot link to itself |
| link | `kind` | `door` (default), `opening` (an archway) or `secret` |
| stair | `room` | the room id the stairs stand in |
| stair | `to_floor` | the NAME of another floor (not an id) |
| stair | `to_room` | optional: a room id on that other floor |
| stair | `label` | optional text |

Every room must be reachable from the first room of its floor through links. That is the rule
models break most often: draw the graph, then check no room is left alone.

The exact JSON Schema is `mapspec.schema.json`. It cannot say the cross-reference rules (link
ends are room ids of that floor, `to_floor` names a real floor, reachability), so the studio
also checks those and sends back the exact complaint.

## 3. How the layout works

- Rooms are placed outward from the first room along a spanning tree of the links, with a
  passage of 2-6 squares between neighbours. Extra links become L-shaped corridors.
- So the first room is the entrance, and a hub room with many links ends up in the middle.
- Size advice: normal rooms 4-10 squares, great halls 10-20, corridors `kind:"corridor"` 2-3
  squares wide and long (for example `w:8, h:2`).
- A floor of more than about 30 rooms may not fit; prefer more floors over huge floors.
- You do not choose where anything goes. Do not write positions, coordinates or directions.

## 4. Names drive furnishing

When the game master furnishes a room, its name picks the props. A name that contains one of
these keywords gets that recipe (the first row that matches wins):

{{FURNISH}}

Corridors get torches, and areas get rocks and bushes. Any other room gets crates, barrels and a
table. So call a sleeping hall "Barracks", not "Room 3".

## 5. Vocabulary the studio has

A MapSpec does **not** take materials, props or lights. The most common invented fields are
`material`, `props`, `furniture`, `x` and `y`. Leave them out: the studio picks them.

For tone and for style words, these exist:

- materials: {{MATERIALS}}
- props: {{STAMPS}}
- lights: one lamp per lit room, at most {{MAX_LIGHTS}} per floor

## 6. What the linter checks after layout

After the layout the studio runs its floor linter on your spec. An error is sent back to you;
warnings and notes are shown to the game master beside the preview.

- `unreachable_room`: a room no door or opening reaches. Link it.
- `secret_only_room` (note): a room reachable only through a secret link. Fine when intended.
- `duplicate_room_name` (note): two rooms with one name. Make names distinct ("Cell 1", "Cell 2").
- `corridor_dead_end` (note): a corridor that joins only one place.
- `door_to_void`: a door that opens into empty space.
- `mark_on_nothing`: a door with no room on either side.
- `mark_inside_room`: a door inside one room, where there is no wall.
- `light_in_void`: a lamp outside every room.
- `space_unused`: a room that ended up with no squares.
- `empty_floor`: nothing at all on the floor.
- `walls_over_cap` (error): more than {{WALL_CAP}} wall segments; nd-world would refuse the publish. Make the map smaller.
- `lights_over_cap` (error): more than {{MAX_LIGHTS}} lights. Set `lit:false` on some rooms.
- `link_to_missing_floor`, `link_to_same_floor`, `link_not_returned`: stairs that lead nowhere, to their own floor, or with no way back.
- Project-only checks, never raised for a fresh spec: `zone_stale_room`, `zone_empty`, `entity_link_stale`, `encounter_in_missing_room`, `marker_link_unpublished`.

## 7. WorldSpec (an overland map)

Every coordinate is a fraction of the map, 0 to 1: `x` left to right, `y` top to bottom.

| Field | Rule |
|---|---|
| `name` | 1-80 characters |
| `cols`, `rows` | 16-512 cells (default 160 x 110) |
| `land` | 0.05-0.95, the fraction that is dry land |
| `continent` | 0-1; 0 = scattered islands, 1 = one central continent |
| `ridges` | 0-1, mountain chains |
| `rivers` | 0-40 |
| `biomes` | up to 30 ellipses `{biome, x, y, rx, ry, density}`; `biome` is forest, meadow, rocky, swamp or desert; `rx`, `ry` 0.02-0.5; `density` 0.5-4, higher means sparser |
| `places` | up to 60 `{name, note, x, y}`: cities, ruins, landmarks; `note` at most 500 characters |
| `regions` | up to 20 `{name, points, color}`: `points` are 3-100 `[x, y]` pairs, `color` is `#rrggbb` |

Biomes grow props only on dry land. Put places on land; near the middle of the map is safest for
islands. A place in the sea is moved or refused. The exact schema is `worldspec.schema.json`.

## 8. Refining a floor

You are given the current floor as a spec, a list of PINNED room ids and a change request.
Reply with the complete revised map spec holding exactly one floor. Pinned rooms must come back
with the same `id`, `name`, `w`, `h` and `kind`. Other rooms may change, move or go; new rooms
need new unique ids; every room stays reachable from the first through links.

## 9. Worked examples

The files in `examples/` each hold a `request`, a short `why` and the `spec` that answers it.
They all validate, lay out and lint without errors. Copy their shape, not their names.

## 10. Common mistakes

See `mistakes.md`.

## 11. When a reply is refused

The studio sends back the exact problems (see `repair.md`). Change only what they name and
reply with the whole corrected object.
