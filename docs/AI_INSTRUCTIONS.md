# AI instructions for agents

The studio ships one **instruction pack** that tells a model how to write a map spec. The
studio's own AI features use it, and so can any outside agent: Claude Code, a script, an MCP
client. It needs no AI to be configured in the studio.

## Where it lives

`crates/ndcart-server/assets/ai/` is the source of truth. It is compiled into the `ndcart`
binary, so a running studio always serves the pack that matches its code.

| File | What it is |
|---|---|
| `pack.toml` | the pack `version`, the `schema_version`, and the list of files |
| `system_map.md`, `system_world.md`, `system_refine.md`, `system_tags.md`, `system_theme_pack.md` | the short system prompts the studio sends |
| `reference.md` | the full reference: fields, limits, how the layout works, what the linter checks |
| `repair.md` | the text the studio sends back when a reply is refused |
| `mistakes.md` | common mistakes and their fixes |
| `mapspec.schema.json`, `worldspec.schema.json` | JSON Schemas (draft 2020-12) |
| `examples/*.json` | worked examples: `{request, spec, why}` |

Placeholders such as `{{MAX_ROOMS}}` in the text files are filled from the real constants when
the pack is served, and tests fail when the pack and the code disagree (every linter code,
material, prop and furnishing keyword is explained; every example validates and lints; the
schemas match the validators). Change the code and the tests tell you which file to update.

## Versioning

`version` in `pack.toml` is semver for the *text*: bump the patch for wording, the minor for new
guidance, the major when the shape of a spec changes. `schema_version` changes only when the
MapSpec or WorldSpec shape does. The served reference ends its title with
`pack <version> · schema <n> · ndcart <app version>`.

## Getting the pack

Over HTTP (the usual token applies):

- `GET /api/ai/instructions?kind=map|world|refine` returns the system prompt, reference,
  repair template, schema and examples as one JSON object.
- `GET /api/ai/schema/mapspec` and `/api/ai/schema/worldspec` return the schemas.
- `POST /api/spec/check` with `{spec, kind?}` runs the checks the studio's own loop runs:
  unknown fields, validation, layout, the floor linter. It returns
  `{ok, kind, errors, unknown_fields, problems}` and changes nothing.

Over MCP (`ndcart mcp`): the tools `get_instructions`, `get_spec_schema` and `validate_spec`,
and the pack files as resources `ndcart://ai/pack/<file>`. See
[PLUGIN_POSTURE.md](PLUGIN_POSTURE.md).

## The external-agent loop

1. Read the instructions (`get_instructions`, or the HTTP route).
2. Write a spec: one JSON object, no positions, no invented fields.
3. Check it with `validate_spec` (or `POST /api/spec/check`). Fix every error it names, then
   check again. Warnings and notes in `problems` are for the game master to see.
4. Send the valid spec to `POST /api/docs/{id}/propose`. It becomes a **proposal**: the game
   master previews it and accepts or rejects it. Nothing changes in a project until they do.

See [MAP_SPEC.md](MAP_SPEC.md) for the human summary of the spec format and
[HTTP_API.md](HTTP_API.md) for the routes.
