# Plugin and automation posture (plan §22.6)

The studio's first automation surface is the **MCP stdio server** (`ndcart mcp`),
and everything it does follows the posture the plan set down for anything that
comes after it (plugins, scripts, assistants).

## What the MCP surface exposes, and why

Eight read-shaped tools — `list_projects`, `get_stats`, `get_floor_spec`,
`lint_floor`, four that teach an agent the spec format (`get_instructions`,
`get_spec_schema`, `validate_spec`; see below), and
`export_png` (renders a PNG, the players' copy by default, to an explicit
`.png` path the caller names; it refuses to overwrite an existing file unless
`overwrite` is true; `export_svg` remains as a deprecated alias for one
release). `ask` is no longer listed: the AI lives in the studio process, so
over MCP it only returns a "not available" error. Writes go
through the HTTP API's validated op endpoints, not through the MCP surface:
an agent that wants to *change* a map works the same way a human does — draft,
preview, accept — so nothing skips validation. Tested end to end
(`tests/mcp_stdio.rs`): the handshake, the tool list, a tool call over a data
dir, and the error shape for unknown methods.

### Teaching an agent the spec, and the external-agent loop

`get_instructions {kind}` returns the AI instruction pack (system prompt, reference, repair
template, JSON Schema, worked examples; `docs/AI_INSTRUCTIONS.md`), and `get_spec_schema {kind}`
the schema alone. `validate_spec {spec, kind?}` runs the same checks the studio's own AI loop
runs (unknown fields, validation, layout, floor linter) and changes nothing; a spec with
problems is a normal answer listing them, not a tool error. The pack files are also MCP
**resources** (`resources/list`, `resources/read`, `ndcart://ai/pack/<file>`), read-only and
compiled into the binary, so no path from the caller is ever opened.

The loop an external agent follows: read the instructions, write a spec, `validate_spec`, fix
what it reports, then send the spec to the HTTP `/api/docs/{id}/propose` so the game master
previews it, and `accept` only on their say-so. MCP never writes a project.

## The rules any future surface must keep

1. **Typed operations, never strings.** A tool or command receives typed
   parameters. Nothing in the document, the settings, or a plugin payload is
   ever interpreted as a shell command or a path to execute.
2. **Same validation as the UI.** Plugin- or agent-generated changes pass the
   same journal-op validation every manual edit passes (finite, bounded,
   referenced ids exist). There is no second, looser path.
3. **Declared capabilities.** A future plugin manifest declares what it reads
   (project text, selected assets), what it writes (explicit output paths),
   and whether it talks to the network — separately. Undeclared access does
   not exist.
4. **Off by default.** Third-party scripts or plugins shipped inside a
   project file stay disabled until the GM enables them; the studio never
   executes code that arrived as content.
5. **Isolation.** Third-party logic runs out-of-process (a worker, a separate
   binary) or behind the same stdio boundary the MCP server uses — never in
   the studio's privileged context.
6. **Namespaced, versioned, retained.** Extension data lives under a
   namespaced key; unknown fields load and save untouched.
7. **Bounded.** Time limits, cancellation and output caps on anything that
   runs code or a long job.

The Tiled documentation's own warning (extensions can access files and run
processes) is the precedent this posture exists to avoid repeating.
