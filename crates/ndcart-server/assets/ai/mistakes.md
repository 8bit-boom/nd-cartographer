# Common mistakes, and the fix

- **Positions or coordinates in a MapSpec** (`"x"`, `"y"`, `"position"`). Leave them out. The
  layout is computed from the links.
- **`props`, `material`, `furniture` or `lights` on a room.** Not fields. A room's name picks its
  props; its `lit` flag picks its lamp.
- **A link to a room on another floor.** Links join rooms of one floor. Use `stairs` for another
  floor.
- **Stairs `to_floor` given as an id.** It is the floor's `name`, spelled exactly.
- **A room over {{ROOM_MAX}} squares across, or under {{ROOM_MIN}}.** Use {{ROOM_MIN}}-{{ROOM_MAX}}. Make a hall 12-20, not 80.
- **More than {{MAX_ROOMS}} rooms on a floor.** Split it into floors.
- **An isolated room.** Every room needs a path from the first room. Add a link.
- **Duplicate ids on a floor.** Ids are unique per floor; names should be distinct too.
- **Prose or code fences around the JSON.** Reply with the object only.
- **`"kind":"door"` on a room.** Room kinds are `room`, `corridor`, `area`. Doors are link kinds.
- **`lit` as a string** (`"true"`). It is a JSON boolean.
- **Numbers as text** (`"w":"8"`). Sizes are JSON numbers.
- **WorldSpec coordinates above 1** (pixels or columns). Every coordinate is 0-1.
- **A WorldSpec place in the sea.** Put places on land, near the middle for an archipelago.
