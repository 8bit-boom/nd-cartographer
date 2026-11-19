# Automation recipes: drafting maps from scripts and schedules

`ndcart draft` makes a spec from a prompt without opening the studio, so a shell loop, cron,
a systemd timer, Windows Task Scheduler or a container can run it. Scheduling stays the
operating system's job: the studio has no scheduler of its own.

Every recipe here is either covered by a test (marked **tested**) or has to be tried on your
machine (marked **manual**). The tests use the built-in stand-in model server, never a real
model. Which model to run is in `LOCAL_AI.md` (written next); how a model is told what to write
is in [AI_INSTRUCTIONS.md](AI_INSTRUCTIONS.md).

## The command

```text
ndcart draft "PROMPT" [--kind map|world] [--seed N] [--template] [--out FILE]
ndcart draft --batch prompts.txt|prompts.jsonl --out-dir DIR [same options]
ndcart draft --check FILE [--kind map|world]
```

| Flag | What it does |
|---|---|
| `--kind world` | Ask for a world map spec instead of a battle map (`--world SLUG` is still the nd-world slug). |
| `--seed N` | The seed for `--template` and for the layout when the result is kept (default 1). |
| `--template` | No model at all: the deterministic template draft. Works with nothing configured. Map specs only. |
| `--out FILE` | Write the spec to a file instead of printing it. |
| `--batch FILE --out-dir DIR` | One spec per prompt: `001-name.json`, `002-name.json`, and `report.json`. Exit code 1 if any prompt failed; the others are still written. |
| `--check FILE` | Run the studio's spec check on a file (no model). Exit code 1 if it is not usable. |
| `--accept-into PROJECT --data-dir D` | Add the drafted floors to a saved project, as the studio's Accept does. `PROJECT` is its id or exact name; `--create` makes it when it does not exist. |

**Which model.** Flags first: `--llm-url URL --llm-model NAME` (a model you run), or
`--server URL --email EMAIL` (nd-world). With neither, the environment is read:
`ND_CARTO_LLM_URL`, `ND_CARTO_LLM_MODEL`, and the key from `ND_CARTO_LLM_KEY` or
`ND_CARTO_LLM_KEY_FILE`; then the studio's saved AI settings in `--data-dir`. The key is never
taken from a command-line argument.

**Batch files.** A `.txt` file has one prompt per line (blank lines and lines starting with
`#` are skipped). A `.jsonl` file has one JSON value per line: a string, or
`{"prompt": "...", "name": "file name hint", "kind": "map" or "world"}`.

**`report.json`:**

```json
{"total": 3, "ok": 2, "failed": 1, "accepted_into": null,
 "items": [{"index": 1, "prompt": "a crypt", "kind": "map", "status": "ok",
            "attempts": 1, "notes": [], "file": "001-a-crypt.json"},
           {"index": 2, "prompt": "...", "kind": "map", "status": "failed", "error": "..."}]}
```

`attempts` is how many times the model was asked (0 for `--template`); `notes` are the floor
linter's findings, which never stop a draft.

Tested: a batch with one failing prompt writes the other files, the report, and exits 1; a
template draft needs no endpoint; `--accept-into` adds floors to a saved project and is
refused while a studio holds the folder.

## A shell loop (manual)

```bash
export ND_CARTO_LLM_URL=http://127.0.0.1:8391/v1
export ND_CARTO_LLM_MODEL=ndcart-local
export ND_CARTO_LLM_KEY_FILE=$HOME/.config/ndcart/llm.key

# One batch call is simpler than a loop, and writes a report:
ndcart draft --batch prompts.txt --out-dir drafts/
# Check what came out, without a model:
for f in drafts/0*.json; do ndcart draft --check "$f" || echo "needs a look: $f"; done
```

A loop of single calls works too (`while read -r p; do ndcart draft "$p" --out "$(echo "$p" | tr -c 'a-z0-9\n' - ).json"; done < prompts.txt`),
but each failure then has to be caught by your own script.

## Keeping the result in a project

```bash
ndcart draft "a crypt with a secret door" --accept-into "Nightly drafts" --create \
  --data-dir /srv/ndcart/data
```

This writes into the project folder directly, so it must not race the studio. `ndcart serve`
holds a lock on `<data>/.ndcart.lock`; `--accept-into` takes the same lock and stops with
"a studio is running" when it cannot. Two ways round it: stop the studio while the job runs,
or draft to files (`--out-dir`) and bring them in through the studio (the **From a spec**
dialog, or the curl recipe below, which talks to the running studio). The lock is released by the
operating system when a process ends, so a crash leaves nothing stale.

## cron (manual)

Put the settings in an environment file the job reads, mode 600:

```bash
# /etc/ndcart/draft.env
ND_CARTO_LLM_URL=http://127.0.0.1:8391/v1
ND_CARTO_LLM_MODEL=ndcart-local
ND_CARTO_LLM_KEY_FILE=/etc/ndcart/llm.key
ND_CARTO_DATA_DIR=/srv/ndcart/data
```

```cron
# Draft a random dungeon every night at 03:17 and keep it in the project "Nightly drafts".
17 3 * * *  . /etc/ndcart/draft.env; export ND_CARTO_LLM_URL ND_CARTO_LLM_MODEL ND_CARTO_LLM_KEY_FILE ND_CARTO_DATA_DIR; ndcart draft "a random dungeon: five to nine rooms, one secret door" --accept-into "Nightly drafts" --create >> /var/log/ndcart-draft.log 2>&1
```

If the studio runs all day on the same data folder the job will be refused; point it at a
folder of its own with `--out-dir` instead, or use the HTTP recipe.

## systemd timer (manual)

```ini
# /etc/systemd/system/ndcart-draft.service
[Unit]
Description=Draft a map with ndcart

[Service]
Type=oneshot
EnvironmentFile=/etc/ndcart/draft.env
ExecStart=/usr/local/bin/ndcart draft --batch /etc/ndcart/prompts.txt --out-dir /var/lib/ndcart/drafts
```

```ini
# /etc/systemd/system/ndcart-draft.timer
[Unit]
Description=Nightly ndcart drafts

[Timer]
OnCalendar=*-*-* 03:17:00
Persistent=true

[Install]
WantedBy=timers.target
```

`sudo systemctl enable --now ndcart-draft.timer`, then `systemctl list-timers` and
`journalctl -u ndcart-draft.service`. A failed prompt makes the service fail (exit 1), so
`systemctl --failed` shows it.

## Windows Task Scheduler (manual)

```bat
schtasks /Create /SC DAILY /ST 03:17 /TN "ndcart nightly draft" /TR "cmd /c \"set ND_CARTO_LLM_URL=http://127.0.0.1:8391/v1&& set ND_CARTO_LLM_MODEL=ndcart-local&& set ND_CARTO_LLM_KEY_FILE=C:\ndcart\llm.key&& C:\ndcart\ndcart.exe draft --batch C:\ndcart\prompts.txt --out-dir C:\ndcart\drafts >> C:\ndcart\draft.log 2>&1\""
```

Whether the quoting survives your shell and Windows version is something to try once with
`schtasks /Run /TN "ndcart nightly draft"`. A small `.bat` file that sets the variables and runs
`ndcart.exe` is easier to get right, and then `/TR C:\ndcart\nightly.bat` is all the task needs.
The Windows build of the studio has not been compiled or run by the author of this
document (see the README).

## Docker (manual)

With the compose file's model profile running (see [DOCKER.md](DOCKER.md)):

```bash
# Draft into files on the data volume, without touching the running studio's projects:
docker compose run --rm -e ND_CARTO_LLM_URL=http://llm:8080/v1 -e ND_CARTO_LLM_MODEL=ndcart-local \
  -e ND_CARTO_LLM_KEY_FILE=/data/ai-runtime.key ndcart \
  ndcart draft --batch /data/prompts.txt --out-dir /data/drafts
```

Run on a schedule from the host's cron or systemd timer. `--accept-into` from a second
container is refused while the `ndcart` service is up, because both would write the same
`/data`; use the HTTP recipe against the running service instead.

## curl: propose, preview, accept (manual)

These talk to a running studio. Add `-H "Authorization: Bearer $ND_CARTO_TOKEN"` when it is
not on loopback.

```bash
S=http://127.0.0.1:8300
# 1. Check a spec (changes nothing):
curl -s $S/api/spec/check -H 'content-type: application/json' -d "{\"spec\": $(cat spec.json)}"
# 2. Hold it as a proposal in project $DOC (nothing changes yet):
PID=$(curl -s $S/api/docs/$DOC/propose -H 'content-type: application/json' \
      -d "{\"spec\": $(cat spec.json), \"seed\": 1}" | jq -r .id)
# 3. Look at it:
curl -s "$S/api/docs/$DOC/proposals/$PID/preview?floor=0" -o preview.png
# 4. Keep it:
curl -s -X POST $S/api/docs/$DOC/proposals/$PID/accept
```

A model can also be asked from the studio: `POST /api/draft` `{prompt}` returns the spec, how many asks it took
and what the linter noticed. The routes are listed in [HTTP_API.md](HTTP_API.md).

## An outside agent over MCP (manual)

`ndcart mcp` lets an agent (Claude Code, another MCP client) read the instructions and check
its own work before anything reaches a project: `get_instructions`, `get_spec_schema`,
`validate_spec`, plus the read tools. The loop an agent should follow is in
[AI_INSTRUCTIONS.md](AI_INSTRUCTIONS.md): fetch the instructions, write a spec, call
`validate_spec`, fix what it names, then hand the spec to a person (or POST it to `/propose`).
