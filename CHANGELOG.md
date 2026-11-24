# Changelog

Notable changes to nd-cartographer. Format follows
[Keep a Changelog](https://keepachangelog.com/); the project uses semantic tags
(`v0.1.0`, …). The detailed milestone log lives in
[docs/ND_CARTOGRAPHER_PLAN.md](docs/ND_CARTOGRAPHER_PLAN.md) §20.

## [Unreleased]

### Fixed
- **The browser UI shipped with JavaScript syntax errors** (literal newlines
  inside three single-quoted strings and an unescaped apostrophe) that broke the
  entire `app.js` script. The studio now boots; a syntax gate, ESLint and a
  Playwright smoke test in CI keep it that way.
- History **revert served stale tiles**: `revert_to` cleared the wall cache but
  not the tile cache. A shared `bust_doc_tiles` helper now invalidates tiles on
  every mutation path (ops, undo, revert).
- **Windows startup panic** when generating an access token: it read
  `/dev/urandom` directly. It now uses the OS CSPRNG (`rand::OsRng`) on every
  platform.
- The **Measure tool was keyboard-dead on battle maps** (`m` was bound twice;
  the first binding won). `m` is now context-aware, and tool shortcuts no longer
  fire through an open modal dialog.
- **Body-size caps depended on route registration order** (axum `Router::layer`
  only wraps earlier routes), leaving Foundry import and others at the 2 MB
  default. The cap now applies to every route.
- **Tabs silently went stale**: a skipped SSE refresh dropped the revision
  forever. The client now remembers a pending rev and catches up on dialog
  close, tab focus and stroke end, and handles the `gone` event.
- Foundry import had **no wall/light caps or coordinate bounds** (a finite 1e300
  became `inf` downstream). It now mirrors the UVTT reader's hardening.
- The table screen's share link **omitted the token**, so a non-loopback TV got a
  401. The link now carries it.

### Added
- **Windows install from a git clone**: `install.bat`, `run.bat`, `update.bat` in
  the repo root. Install builds from source when Rust is present, otherwise
  downloads the latest prebuilt Windows binary; run starts the studio and opens
  the browser; update pulls and rebuilds. Maps stay in a portable `data` folder.
- **Auto-save**: a durable-save watermark (`synced_rev`), a `POST /api/docs/{id}/save`
  force-flush and `GET …/save-status`, a 30 s background flush of dirty documents,
  and a UI "✓ saved / ● unsaved" indicator that saves ~1.5 s after editing pauses
  and on Ctrl+S. Bounds the power-loss window without fsyncing every stroke.
- **Optimistic concurrency**: `POST /ops` accepts `base_rev` and answers 409 when
  the map moved on, instead of interleaving two windows' edits.
- **Rename and delete projects**: `PATCH`/`DELETE /api/docs/{id}`, wired into the
  gallery (with confirmation).
- **DNS-rebinding guard** on loopback (Host-header validation).
- **Touch editing**: `touch-action: none`, two-finger pinch zoom, and on-screen
  zoom buttons.
- **Error toasts** (`role="alert"`) separate from the disposable tool-hint line,
  a boot-failure retry, and surfaced stamp/search failures.
- **Mipmapped underlays** (no more aliasing when a large picture is minified).
- **Caches**: prepared strokes cached across tiles, wall cache pruned to the
  latest revision, an LRU decoded-asset cache, a bounded byte cache, and a
  revision-keyed player-preview cache (the table screen no longer re-bakes the
  whole floor per water frame).
- **Supply-chain CI** (`cargo-deny` + `cargo-audit`), a **release-mode bench
  gate**, parser **mutation robustness tests**, and `cargo-fuzz` targets.
- `SECURITY.md` (the threat model) and this changelog.

### Changed
- The tile baker **range-queries the grid** by the tile's cell rect instead of
  scanning every cell per tile.
- UVTT export bakes **off the async threads** without holding the project lock
  (like PNG export).
- Shell files (`/`, `/app.js`, `/app.css`) are served with `no-cache` + ETag.
- The table screen sizes its resolution to the display, backs off on disconnect,
  pauses water frames when hidden, and offers fullscreen + wake-lock.
- A `ResizeObserver` keeps the canvas bitmap matched to its box on layout changes.

### Security
- Upgraded dependencies to clear the RustSec advisories the new supply-chain gate
  found: `zip` 2.6 → 9.8 (six extraction advisories; the studio only *writes*
  `.ndmapz` bundles, but the patched crate removes the risk entirely), `url`
  2.5.8 → 2.5.9, `cookie` 0.18.2 → 0.18.3.

### Notes
- The background journal writer suggested by the audit was **deliberately not
  done**: the fast path already throttles fsync, and moving persistence off the
  request risks the kill-9 durability invariant the project validates. The heavy
  wall derivation is cached instead.