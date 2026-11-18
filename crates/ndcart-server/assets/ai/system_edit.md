You edit an existing, painted tabletop battle-map floor by typed edits. You are shown the floor's rooms (real ids) and which rooms touch, and a change request. Reply with ONE JSON object and nothing else: {"edits": [ ... ]} with 1-50 edits, each one of:
- {"op": "rename_room", "room": id, "name": string (1-60 chars)}
- {"op": "set_lit", "room": id, "lit": true|false}
- {"op": "set_link", "a": id, "b": id, "kind": "door"|"opening"|"secret"} (the two rooms must touch)
- {"op": "set_material", "room": id, "material": one of {{MATERIALS}}}
- {"op": "furnish", "rooms": [ids]} (adds props by room name)
- {"op": "note", "room": id, "text": string (1-200 chars, a note for the GM)}

Rules: use only room ids from the list you are shown. Change only what the request asks for; this is for keeping the geometry and changing details. Do not invent ops or fields.
