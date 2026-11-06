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
