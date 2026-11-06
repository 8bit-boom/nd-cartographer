# Using AI in the studio

Everything works without AI. When it is set up, you can describe a map in words,
and the studio turns the answer into a **preview you accept or reject**. The model
never draws or places anything itself: it writes a [spec](MAP_SPEC.md), and the
studio's own code lays it out.

## Setting it up

The studio reads its settings from its own environment (never from a prompt or a
request), and `ndcart doctor` shows what it found. Choose one:

**nd-world** (its image queue and the world's own place names are available):

```bash
ND_CARTO_SERVER=https://my-nd-world.example  ND_CARTO_EMAIL=gm@example.com \
ND_CARTO_PASSWORD_FILE=/run/secrets/ndcart  ndcart serve --data-dir ~/ndcart-data
```

Optional: `ND_CARTO_WORLD=<slug>`, `ND_CARTO_MODEL=<name>`, `ND_CARTO_TOTP`.

**A model you run yourself** (any OpenAI-compatible server, such as Ollama's `/v1`):

```bash
ND_CARTO_LLM_URL=http://127.0.0.1:11434/v1  ND_CARTO_LLM_MODEL=llama3.1 ndcart serve ...
```

Optional key: `ND_CARTO_LLM_KEY` or `ND_CARTO_LLM_KEY_FILE`. Requests go one at a
time. A direct model writes specs but cannot make pictures or read the world's
place names.

The dialogs show which one is in use (🌐 nd-world, 🏠 your own model, ⛔ none).

## What you can ask for

| Where | What | Result |
|---|---|---|
| **✨ From a spec… → ✨ Draft with AI** | "abandoned metro station, 3 levels, flooded bottom" | a preview of each floor; **Accept** adds new floors with stairs, **Another layout** re-rolls, **Reject** discards |
| same, in *World map* mode | "a cold archipelago kingdom, three political regions" | a preview of the world; Accept replaces the terrain (undoable) |
| outline **Refine…** | "add a barracks next to the entrance" | the floor is described to the AI with the selected rooms *pinned*; pinned rooms must come back unchanged; the result is a **new floor** to preview |
| **🎨 Style…** | "gritty cyberpunk, neon and rust" + room colours | the tone goes into every draft; the palette colours generated rooms |
| *use the place names already in my world* | (checkbox, nd-world) | drafts reuse your existing locations' names where they fit |
| **🖼 Make art…** (nd-world) | a tiling material, a set of props, or a map underlay | pictures arrive in the background; **Keep as material / prop / underlay** under the ones you like |

If the model's reply breaks a rule, the studio sends the exact complaint back and
asks again (twice at most) before giving up with the last problem. Nothing is
ever added silently.

## Staying in control

- Every AI result is a proposal you can look at first; accepting is one undo step
  (floors: one "Undo floors" step each).
- Kept pictures remember their prompt and seed; AI-made art is marked as such.
- The studio does not send your maps to the model: only your description, the
  spec being revised, your style words, and (if you tick it) the names of your
  world's locations.

## Not covered

How good a particular model's maps are depends on the model; the studio has been
tested against stand-ins that speak the same protocols, not against real models.
Not built yet: rooms linked to world entities, upscaling, a style-reference image
for sets, multi-turn conversation, and natural-language questions about your maps
(search is by keyword).
