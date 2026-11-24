# Security

nd-cartographer is a local-first tool: it runs on your machine, holds your maps,
and (optionally) your nd-world credentials and AI keys. This file states the
threat model and the controls that are actually in the code (plan §17.7).

## Reporting a vulnerability

Please open a private security advisory on GitHub
(`Security → Report a vulnerability`) rather than a public issue. There is no
bug bounty; this is a small independent project.

## Threat model

The studio is a single-user, local-first server. The realistic adversaries are:

- **A malicious web page** trying to reach the loopback server (DNS rebinding).
- **A hostile file** — an imported UVTT / Foundry / Dungeondraft pack, or a
  corrupt project journal — trying to crash the server or exhaust memory.
- **A LAN neighbour** reaching a non-loopback listener without permission.
- **An AI model's output** trying to smuggle invalid geometry or secrets.

It is *not* a multi-tenant service: there are no per-user accounts, and anyone
with the token (or loopback access) is the GM.

## Controls in place

**Network access**
- Non-loopback listeners require a token (`Bearer`, cookie, or `?token=`),
  compared in constant time; the cookie is `HttpOnly` + `SameSite=Strict`.
  `/health` is public. The token comes from the OS CSPRNG on every platform.
- Loopback listeners validate the `Host` header against loopback names, so a
  rebinding page sending `Host: evil.example` is refused (403).
- Credentials are read from the environment (`ND_CARTO_PASSWORD[_FILE]`,
  `ND_CARTO_LLM_KEY[_FILE]`, `ND_CARTO_TOKEN[_FILE]`), never from arguments, and
  are never printed or logged. The AI settings API redacts the key.

**Untrusted input**
- Every number crossing a boundary is validated: finite, typed, bounded (the
  "D1 lesson" — a `NaN` once bricked a live map). Refused ops are never journaled.
- Import parsers cap counts and coordinates (UVTT and Foundry wall/light caps,
  coordinate bounds so a finite-but-astronomical number cannot become `inf`),
  and image decode enforces pixel/side/allocation budgets.
- Asset ids are content-addressed (SHA-256) and validated, which also keeps
  `../` out of filesystem paths. Saves are temp-file + fsync + rename.
- Tile coordinates are bounded so a wild value cannot overflow the baker.
- The parsers have hostile-input tests, a deterministic mutation robustness
  suite (stable CI), and `cargo-fuzz` targets (nightly) under `fuzz/`.

**Secrets in output**
- One secrets filter (`for_audience`) strips secret doors and GM-only labels from
  every player-facing output: preview, exports and publish all use it, with tests
  asserting no secret survives.

**Supply chain**
- `cargo-deny` (licenses, advisories, bans, sources) and `cargo-audit` run in CI.
- The browser UI ships no third-party runtime dependencies (nothing from a CDN).

**AI**
- AI output is a *proposal*: previewed and accepted/rejected by the GM, never
  applied unasked. Generated assets carry provenance (model, prompt, seed, date).

## Known limits

- The loopback `Host` guard is a rebinding defence, not authentication: any
  process on the same machine can still reach the server. Run non-loopback with
  a token if the machine is shared.
- The table-screen share link carries the token in the URL (it must, to reach a
  second device); treat that link as a credential.
- Desktop installers are not yet signed.