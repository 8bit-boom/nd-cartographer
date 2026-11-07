# Art packs and your own pictures

## Importing a pack

**📦 Import pack…** in the studio (or `ndcart import-pack FILE_OR_FOLDER --data-dir DIR`)
adds art to the palettes:

- a **Dungeondraft pack** (`.dungeondraft_pack`, a Godot 3 archive): pictures under
  `objects` become **props**, pictures under `terrain` become **materials**;
- a **folder** of pictures: under `objects` or `symbols` → props; under `terrain`,
  `tiles`, or directly under `textures` → materials.

Sizes follow Dungeondraft's convention of 256 pixels to a square. Walls, paths,
lights and thumbnails in a pack are left out. Importing the same pack twice
changes nothing. Archives made by a newer Dungeondraft (a different Godot
archive format) and zip files are refused with advice: unpack them and import the
folder.

Each imported item remembers `from pack <name> by <author>`. The studio never
redistributes art: packs stay in your own data folder, under their own licenses.
Check a pack's license before sharing a map that uses it.

> The archive reader follows the Godot 3 format and is tested on archives built to
> that format; it has not been tried on a wide range of real packs. If one fails,
> `ndcart doctor` and the message shown say what was wrong; unpacking to a folder
> always works.

## Your own pictures

Any stored picture can become:

- a **material**: made seamless by a wrap-around blend so it tiles without a seam
  (`POST /api/assets/{id}/make-material`);
- a **prop**: cut out of a flat background (the colour is guessed from the corners,
  or given), feathered, trimmed, with a soft shadow
  (`POST /api/assets/{id}/make-stamp`).

Pictures made by AI (**🖼 Make art…**) go through the same steps and keep their
prompt and seed. Props can carry tags you confirm; the AI can suggest them but
never applies them itself.

Materials are used as a room's material and props as stamps; both appear in the
normal palettes, render the same in tiles and in exports, and are stored under
`<data dir>/assets/` with their list in `custom.json`.

## Pack metadata v2 (`ndpack.toml`, plan §22.1)

An asset can now carry structured metadata: a **footprint** (the cells its
shape covers, so furnishing never stacks props or blocks a doorway with a
wide table), a **clearance** (free space kept around it), **placement**
(surface, prefers-a-wall, may-mirror), **style tags**, and a **licence
block** (map-commercial / redistribution / attribution, plus the terms).

A pack author provides it with an `ndpack.toml` at the pack root:

```toml
[license]
identifier = "CC-BY-4.0"
commercial_map = "allowed"      # allowed | forbidden | unknown
redistribution = "forbidden"
attribution_required = true

[assets."objects/table.png"]
clearance = 0.5
footprint = [[-1.5, -0.5], [1.5, -0.5], [1.5, 0.5]]

[assets."objects/table.png".placement]
align_to_wall = true
```

On import: a `LICENSE.txt` in the pack becomes the licence terms text, the
pack name drafts style tags (you confirm or edit them), and `ndpack.toml`
overrides per asset. A malformed manifest is reported and skipped — the
pictures still import. Exporting your own art (**⬇ My art pack**) writes the
same manifest back out, so a pack round-trips.

Furnishing uses the metadata: a prop's footprint is its collision box and its
clearance widens the keep-out ring around doors, so generated rooms respect
real shapes, not just one cell per prop. `ndcart publish` lists in its report
every pack whose art is baked into the map, with its licence block — check it
before sharing (the studio surfaces licences; it is not legal advice).

The API is additive: stamps and materials carry `meta`, and
`POST /api/assets/{id}/meta` sets it. Everything defaults, so older packs,
older `custom.json` files and hand-made folders load exactly as before.
