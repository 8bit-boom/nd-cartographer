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

**A model you run yourself** (any OpenAI-compatible server: **Unsloth Studio**,
Ollama's `/v1`, llama.cpp's server, ...). Two ways to point the studio at it:

*In the studio* — open **⚙ Settings**, fill in the endpoint, pick the model from
the dropdown (it reads the endpoint's own model list), optionally an API key,
then **Test connection** and **Save AI settings**. Saving takes effect
immediately — no restart — and the settings live in
`<data dir>/ai-settings.json`. The key is stored there but never sent anywhere
except to the model endpoint itself, and never shown again in the UI.

*In the environment* (CLI and CI keep working this way):

```bash
ND_CARTO_LLM_URL=http://127.0.0.1:11434/v1  ND_CARTO_LLM_MODEL=llama3.1 ndcart serve ...
```

Optional key: `ND_CARTO_LLM_KEY` or `ND_CARTO_LLM_KEY_FILE`. Environment
variables win over saved settings per field. Requests go one at a time. A direct
model writes specs but cannot make pictures or read the world's place names.

**Unsloth Studio** (a local server for Unsloth fine-tuned GGUF models):

1. Start Unsloth Studio and load a model (any instruct model works; bigger is
   better at following the JSON spec shape).
2. In the studio's ⚙ Settings set the endpoint to its OpenAI-compatible address
   (for example `http://127.0.0.1:3915/v1`), press the model dropdown to load
   the served models, pick one, **Test connection** (the model should reply
   "OK"), then **Save**.
3. Use ✨ **From a spec…** to draft, outline **Refine…** to edit a floor while
   keeping the rooms you pinned, and ⚙ **Ask** to search your maps — all
   against your local model.

Small local models: the spec is strict JSON and the studio re-asks with the
exact complaint twice before giving up, so 4B-class models usually work; if a
draft keeps failing, try a model with stronger instruction following.

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
