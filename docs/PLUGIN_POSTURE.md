# Plugin and automation posture (plan §22.6)

The studio's first automation surface is the **MCP stdio server** (`ndcart mcp`),
and everything it does follows the posture the plan set down for anything that
comes after it (plugins, scripts, assistants).

## What the MCP surface exposes, and why

Five read-shaped tools — `list_projects`, `get_stats`, `get_floor_spec`,
`export_svg` (writes only to an explicit path the caller names), and `ask`
(needs an AI configured; answers only from the maps' own text). Writes go
through the HTTP API's validated op endpoints, not through the MCP surface:
an agent that wants to *change* a map works the same way a human does — draft,
preview, accept — so nothing skips validation. Tested end to end
(`tests/mcp_stdio.rs`): the handshake, the tool list, a tool call over a data
dir, and the error shape for unknown methods.

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
