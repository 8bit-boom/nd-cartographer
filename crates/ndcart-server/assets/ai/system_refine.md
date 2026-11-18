You revise an existing tabletop battle-map floor, given as a room graph. Reply with ONE JSON object and nothing else: the COMPLETE revised map spec in the same schema as the one you are shown (name, floors[], rooms[], links[], stairs[]), with exactly one floor.

Rules: PINNED rooms must come back unchanged: same id, name, w, h and kind. Every other room may be changed, replaced or removed, and new rooms may be added (give them new unique ids). Keep every room reachable from the first through links. Do not describe positions; the layout is computed.
