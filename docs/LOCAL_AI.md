# Running a model on your own computer

This guide sets up a local AI for the studio's draft, refine and edit buttons: nothing leaves
your machine, there is no account and no fee. It is optional. The studio works fully without
it, and with no model at all the **template draft** still makes a simple map from your words.

> **Read this first: what has and has not been checked.** The code that downloads, starts and
> tests a model is tested against stand-in servers (a fake model, a fake `llama-server`, a
> fake Hugging Face Hub). **No real model has been run through the studio**, no real
> Hugging Face download has been made, and the Windows and macOS builds have not been compiled
> or run by the author. The model names and file sizes below were written without access to
> the Hugging Face Hub; the first `ndcart ai pull` checks them against the live listing and
> says plainly if one is wrong. The table in section 4 marks every model *not tested*, and
> `ndcart ai models` will say "tested here" only for a model whose self-test you ran on your
> own computer. [Section 11](#11-what-is-and-is-not-verified) lists every claim in this guide
> that has not been confirmed.

## 1. What runs where

Three separate things are involved, and only two of them are on your computer:

| Piece | What it is | Who provides it |
|---|---|---|
| **The model file** | A quantised model in the GGUF format, a few GB. | [Unsloth](https://unsloth.ai) publishes GGUF files of popular open models on Hugging Face (`unsloth/<Name>-GGUF`). |
| **The runtime** | `llama-server`, from llama.cpp: loads a GGUF and answers OpenAI-style requests on a local port. | The llama.cpp project. You install it with your system's package manager or Docker. |
| **The studio** | Asks the runtime to write a *spec* (rooms and how they connect), checks it, lays it out itself, and shows you a preview. | This program. |

**Unsloth Studio is not needed.** Unsloth Studio is a separate application that has its own
web page and bundles its own copy of llama.cpp; if you already use it, the studio can talk to
it like any other OpenAI-compatible server (section 6), but nothing here requires it. The
`unsloth` Python package is only for making your own fine-tuned model (the appendix).

The model never draws anything. It writes a small JSON spec; the studio's own code validates
it, lints it, lays it out and shows a preview you accept or reject
([how the model is instructed](AI_INSTRUCTIONS.md), [the spec](MAP_SPEC.md)). If the reply
breaks a rule, the exact complaint is sent back and the model is asked again, twice at most.

## 2. Quick start: `ndcart ai setup`

```bash
ndcart ai setup
```

It looks at this computer (memory, graphics memory, free disk), finds `llama-server`, lists
the models that fit, and asks which to use. It then downloads the one you pick after showing
its exact size, starts `llama-server` on `127.0.0.1` with a private key file, saves the
settings and runs the self-test (section 8). Useful options: `--model ID` (skip the question),
`--yes`, `--max-gb N` (refuse a bigger download), `--port N`, `--autostart` (have `ndcart serve`
start the model with it), `--no-start`.

If `llama-server` is not installed it stops and prints how to install it (section 3). **The
studio never downloads or installs a program.** It downloads one kind of file only, the GGUF
model, and only from the Hugging Face Hub.

The same steps, one at a time, with `--data-dir` pointing at your studio's data folder (or set
`ND_CARTO_DATA_DIR`):

```bash
ndcart ai models              # the catalog, marked by what fits this computer
ndcart ai pull qwen3.5-4b     # size shown first and confirmed; SHA-256 checked; resumes
ndcart ai serve               # start llama-server (loopback only); `ndcart ai stop` stops it
ndcart ai status              # runtime, models, server, endpoint, what the endpoint supports
ndcart ai test                # the self-test: good, usable or too weak
ndcart ai eval                # the full scorecard (section 8)
```

You can also do it in the studio: **⚙ Settings → Run a model on this computer** has the same
steps as buttons, with the download size asked first.

## 3. Installing the runtime (`llama-server`)

Pick your system. After installing, `llama-server --version` should print a version; if the
studio cannot find it, set `ND_CARTO_LLAMA_SERVER` to its full path.

| System | Command | Notes |
|---|---|---|
| macOS (Apple Silicon or Intel) | `brew install llama.cpp` | Apple Silicon uses Metal. |
| Linux | `brew install llama.cpp`, or `conda install -c conda-forge llama.cpp` | The conda package has CUDA and Vulkan builds. Your distribution may package it too. |
| Windows | `winget install llama.cpp` (then open a new terminal) | The zip files on the [llama.cpp releases page](https://github.com/ggml-org/llama.cpp/releases) are the alternative; pick the CUDA or Vulkan build for your card. |
| Docker or TrueNAS | the compose profiles, section 7 | No install on the host. |

(manual) The commands above come from llama.cpp's install documentation; they have not been
run by the author of this guide on every system.

## 4. Picking a model

The studio needs a model that can follow a strict JSON format. Bigger is steadier, but a
4B-class model is a reasonable first try. Sizes are approximate; `ndcart ai pull` shows the
real size before it downloads anything. **None of these has been tested with the studio by its
author.**

| Your computer | Model (`ndcart ai pull ID`) | Quant | About | Notes |
|---|---|---|---|---|
| 8 GB of memory, no GPU | `qwen3.5-4b` | UD-Q4_K_XL | 3 GB | The suggested first try. |
| Old laptop | `qwen3.5-2b` | Q4_K_M | 1.6 GB | Last resort; expect "usable" at best. |
| 8 GB, to compare | `gemma-4-e4b` | Q8_0 | 5 GB | A different model family. |
| 12-16 GB graphics card, or a 16 GB Mac | `qwen3.5-9b` | UD-Q4_K_XL | 5.9 GB | Likely the best quality for the size. |
| same | `gpt-oss-20b` | UD-Q4_K_XL | 12 GB | A reasoning model: see "thinking" in section 9. |
| 24 GB or more | `qwen3.6-35b-a3b` | UD-Q4_K_XL | 22 GB | Mixture of experts: fast for its size. |
| same | `qwen3.8-27b` | UD-Q4_K_XL | 18 GB | A dense model. |
| same | `gemma-4-31b` | UD-Q4_K_XL | 18 GB | A second family to compare. |

Rules of thumb, none of them measured here: the file plus working memory must fit in your RAM
(or graphics memory for speed); on a Mac the GPU can use roughly two thirds to three quarters
of unified memory; a mixture-of-experts model decodes much faster than a dense model of the
same file size. `ndcart ai models` applies a simple version of this (file size × 1.2 plus the
context's memory) to mark what "fits".

The status column of `ndcart ai models` shows **not tested** until you run `ndcart ai test`
for that model on your computer; only then does it say "tested here: good" (or usable or too
weak). That record lives in `<data dir>/ai-verified.json` and is per machine on purpose.
Run `ndcart ai eval --repeat 3` on two candidates and keep the one with the higher valid and
first-try percentages; speed is the tiebreak.

The catalog's repository and file names (`unsloth/Qwen3.5-4B-GGUF` and so on) were written
without live Hub access. If `ndcart ai pull` says a file is not in the repository it lists
what is there, and `ndcart ai pull unsloth/Repo-GGUF:File.gguf` fetches an exact file. A repo
that is not `unsloth/*-GGUF` is refused unless you add `--allow-non-unsloth`.

## 5. Running it yourself

Everything `ndcart ai serve` does can be done by hand. The managed command is:

```text
llama-server -m <model.gguf> --host 127.0.0.1 --port <p> --alias ndcart-local \
             --api-key-file <data>/ai-runtime.key -c <ctx> -np 1 --no-webui
```

By hand, letting llama.cpp download the model itself (it does not confirm the size or check a
checksum, which is why the studio's own `ai pull` exists):

```bash
llama-server -hf unsloth/Qwen3.5-4B-GGUF:UD-Q4_K_XL --no-mmproj \
  --host 127.0.0.1 --port 8391 --alias ndcart-local -c 16384 \
  --api-key-file ~/.config/ndcart/llm.key
```

(manual) Then point the studio at it, in **⚙ Settings** (endpoint `http://127.0.0.1:8391/v1`,
pick the model, **Test connection**, **Save**) or with the environment:

```bash
ND_CARTO_LLM_URL=http://127.0.0.1:8391/v1 ND_CARTO_LLM_MODEL=ndcart-local \
ND_CARTO_LLM_KEY_FILE=~/.config/ndcart/llm.key ndcart serve --data-dir ~/ndcart-data
```

Notes: always pass `--port` (the default has changed between llama.cpp releases); `--host
127.0.0.1` keeps it off your network; the key goes in a file, not on the command line; **16384**
tokens of context is enough (the instructions take about 3-6K and a large reply 3-5K), and 32768
helps long refinements. Some model families (Qwen3.5 small models) ship an image component that
`-hf` downloads too; `--no-mmproj` skips it, and the studio does not use images.

**Ollama** (manual): `ollama run hf.co/unsloth/<Repo>-GGUF:<QUANT>`, endpoint
`http://127.0.0.1:11434/v1`. Unsloth's own notes say its Qwen3.5 GGUFs do not currently work
in Ollama; use another model there or use llama.cpp.

**Unsloth Studio** (manual): its OpenAI-compatible API is on the port you gave it
(`unsloth studio -p 8888` gives `http://127.0.0.1:8888/v1`); create a key in its Settings → API
and paste it into the studio's API key field.

**LM Studio and others:** anything that speaks the OpenAI `/v1/chat/completions` protocol works
the same way; the studio treats it as "a model you run yourself".

## 6. How the studio uses it

- **Settings panel.** ⚙ Settings has two parts: the endpoint form (address, model, key, with
  **Test connection**) for a server you run, and **Run a model on this computer** for the
  managed one (download, start, stop, self-test, use). Saving takes effect at once;
  settings live in `<data dir>/ai-settings.json` (the key is stored there with mode 0600 and
  never shown again). Environment variables win over saved settings, field by field.
- **What it asks for.** One request at a time. For servers that support it, the studio asks for
  JSON that follows the schema (so the reply is always well-formed), turns thinking off for
  JSON tasks, and falls back to a plain request once if the server rejects the schema.
  Whatever the server does, the reply is still checked, linted and, if needed, repaired.
- **Large requests.** A request for several floors is planned first (an outline), then each
  floor is drawn on its own, so a small model does not have to hold the whole map at once.
- **Without a model.** The template draft, `ndcart draft --template`, and `--check` need no
  model. See [AI_RECIPES.md](AI_RECIPES.md).
- **External agents.** The same instructions, schemas and checks are served over HTTP and
  `ndcart mcp` for tools outside the studio; see [AI_INSTRUCTIONS.md](AI_INSTRUCTIONS.md).

## 7. Docker and TrueNAS

The compose file has two opt-in profiles, `ai-cpu` and `ai-gpu`, that run
`ghcr.io/ggml-org/llama.cpp:server` (or `:server-cuda`) next to the studio. The steps (download
the model with the studio image, make the key, start the profile, point the studio at it) are
in [DOCKER.md](DOCKER.md#a-model-on-the-same-machine-optional-the-ai-cpu-and-ai-gpu-profiles).
CI checks that the compose file is valid for each profile; **it has not been run with a GPU or
on a TrueNAS machine.**

## 8. Testing a model: `ai test` and `ai eval`

**`ndcart ai test`** (about a minute on a good machine) checks, in order: the server is ready,
the model name is served, it answers a ping, it answers a small JSON-schema request, and
three golden drafts go through the real validate, lint and repair loop. `--tools` also probes
tool calling (not needed by the studio). The verdict:

| Verdict | Meaning | Exit code |
|---|---|---|
| **good** | all three drafts valid on the first ask | 0 |
| **usable** | all valid, but some needed repairs | 0 |
| **too weak** | at least one never became valid | 3 |
| *(could not test)* | the endpoint did not pass the basics | 1 |

**`ndcart ai eval`** is the full scorecard: 22 prompts in four sets that you can run separately
with `--set`: `map` (8 single-floor maps, the first three are the self-test's), `multi` (4
requests for 2-3 floors), `world` (4 overland maps), and `edits` (3 floor revisions that must
keep pinned rooms, 3 typed edit plans). Each run is scored on:

- **valid**: the loop produced a spec the studio accepts;
- **first try**: valid without a repair round;
- **checks**: how many of the things the request asked for are in the result (floor count,
  room count, a room with a matching name, a secret door, stairs, places, regions, pinned
  rooms unchanged). A run that never became valid scores zero;
- **lint warnings**, **asks** to the model, **seconds**, and **tokens per second** (from one
  short probe request before the run).

```bash
ndcart ai eval                              # all 22 cases against the configured model
ndcart ai eval --set multi --repeat 3       # three runs of each multi-floor case
ndcart ai eval --url http://127.0.0.1:8391/v1 --model ndcart-local --json --out report.json
ndcart ai eval --markdown > eval.md         # a table to paste into notes
ndcart ai eval --record --id qwen3.5-4b     # remember the verdict for this computer
```

The overall verdict uses the same words: **good** = every run valid and at least 80% right on
the first ask; **usable** = at least 80% valid; otherwise **too weak** (exit codes as above).
`--record` writes the verdict only for a full run (`--set all`) of a catalog model. The layout
is seeded, so the same spec always lays out the same way; the model's own replies are not
reproducible, so use `--repeat` before comparing two models.

For a way to try all of this with no model, `ndcart ai stand-in --port 8099` is a fake model
server that replays the instruction pack's worked examples; it is how the studio's own tests
run. Its scores say nothing about a real model.

**Results for real models: none have been measured.** When you run `ai eval` on a model,
record the date, the model, the quant and the report alongside your notes; this guide
deliberately has no numbers of its own.

## 9. Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| "cannot reach" / connection refused | The server is not running or the port differs. `ndcart ai status` shows what the studio expects; start it with `ndcart ai serve`, or check `--port`. The default port of `llama-server` has changed between releases: always pass `--port`. |
| Self-test says the server is "still loading" (503) | A big model takes a while to load. Wait and run again. |
| "the server refused the API key" (401) | The key file differs from the one the server was started with. For the managed server the key is `<data>/ai-runtime.key` (`ndcart ai key` prints where). For your own server set `ND_CARTO_LLM_KEY` or `ND_CARTO_LLM_KEY_FILE`, or the key field in Settings. |
| "the model used its whole reply for thinking" or "returned an empty reply" | A reasoning model spent its answer on thinking. The studio already turns thinking off for JSON requests on llama.cpp; if yours still does it, start the server with `--reasoning-budget 0` or use a non-thinking model, or turn on thinking in settings only if you raise the reply limit. For gpt-oss, a low reasoning effort is the setting to look for. |
| Replies cut off (`reply cut off: max_tokens`) or "context" errors | Raise `-c` (for the managed server: `ndcart ai serve --ctx 32768`). Large multi-floor requests are the usual cause. |
| Out of memory, or the server dies while loading | Use a smaller model or quant, lower `-c`, or put fewer layers on the GPU (`--gpu-layers N`, or `-ngl N` by hand). |
| Very slow | Running on the CPU. `ndcart ai status` and `llama-server --list-devices` show whether a GPU is used; a CPU-only llama.cpp build never uses one. |
| The server rejects the schema request | Some servers ignore or reject `response_format`. The studio falls back once to a plain request; drafts then rely on the repair loop, so expect more asks. |
| `ai pull` says the file is not in the repository | The catalog names were written without live Hub access. Read the listing it prints and use `ndcart ai pull unsloth/Repo-GGUF:File.gguf`. |
| A checksum mismatch | The download is deleted. Run it again; if it repeats, the file changed on the Hub or the connection is altering it, so do not use it. |
| Windows or macOS problems | Those builds have not been run by the author. `ndcart doctor` checks the runtime, the model's checksum, the server, the GPU and disk and memory. |

## 10. Privacy and safety

- With a local model, **your descriptions, specs and style words go only to the server on your
  machine.** The studio sends the model your request, the spec being revised, your style words,
  and (if you tick it, nd-world only) your world's place names. It does not send your maps.
- The one thing downloaded is the **model file** (GGUF), over HTTPS from `huggingface.co` and
  its download hosts, after you confirm the size, and checked against the checksum the Hub lists.
  **No program is ever downloaded or installed by the studio**; `llama-server` comes from your
  package manager, the llama.cpp releases you choose, or Docker.
- `HF_TOKEN` (for gated models), the model key and passwords are read only from the
  environment or private files, never from command-line arguments, and are not logged.
- The managed server listens on `127.0.0.1` only and asks for a key on every request. The key is
  passed as a file path, never as a value.
- The model cannot change your maps by itself: everything it proposes is a preview, accepting
  is one undo step, and its replies are validated before you see them.
- A GGUF is data, not a program, but a model file you did not make is still something to get
  from a source you trust. `ndcart ai import FILE.gguf` copies a file you already have into the
  models folder without contacting anything.

## 11. What is and is not verified

Confirmed by tests in this repository: the catalog, hardware and fit logic, the downloader
(sizes, checksums, resume, refusing other hosts) against a fake Hub; starting, stopping and
supervising a fake `llama-server`; the self-test and the eval against the stand-in model
server, including repairs, bad replies and a model that never answers; the instruction pack
matching the code; the commands and examples in [AI_RECIPES.md](AI_RECIPES.md) marked tested.

**Not confirmed** (nobody has run these):

- any real model's output, quality, speed or memory use, with this studio;
- that the catalog's Hugging Face repositories, file names and sizes exist as written;
- the exact install commands per operating system, the `-hf` one-liners, and the behaviour of
  Ollama and Unsloth Studio with these models;
- the Windows and macOS builds, the desktop app, and GPU use of any kind;
- the Docker model profiles on a real GPU and on TrueNAS;
- the numbers in the "about" column of the table in section 4;
- that llama.cpp's `--reasoning-budget 0` turns thinking off for every model family;
- anything marked (manual).

## Appendix: making your own model (advanced, optional)

Nothing in the studio depends on this. It is for someone who wants a small model that is better
at this one task than a general one. It needs Linux or Windows with WSL, an NVIDIA GPU, Python,
and patience; none of the steps below has been run by the author of this guide.

1. **Make training pairs.** A pair is a request and the spec that answers it. Good sources:
   the worked examples in the instruction pack (`crates/ndcart-server/assets/ai/examples/`);
   specs from `ndcart draft --template` for simple requests; and specs from a stronger model
   run through `ndcart draft --batch`. **Keep only specs that validate and lint clean:**
   `ndcart draft --check FILE` exits 0 for a usable one and prints the linter's findings;
   discard any with warnings you would not accept. Hand-written prompts that vary the wording
   matter more than volume.
2. **Hold some out.** Keep the 22 cases of `ndcart ai eval` out of the training data so the
   scorecard stays honest.
3. **Fine-tune** with the `unsloth` package (see its documentation for the current notebook
   and arguments):

   ```bash
   uv pip install unsloth --torch-backend=auto
   ```

   Fine-tune a small base model (a 4B-class model is plenty) on your pairs, formatted as the
   chat template of the base model expects: the pack's system prompt, then the request, then
   the spec as the answer.
4. **Export a GGUF** from the same script:

   ```python
   model.save_pretrained_gguf("my-cartographer", tokenizer, quantization_method="q4_k_m")
   ```

5. **Bring it into the studio:**

   ```bash
   ndcart ai import my-cartographer/model-q4_k_m.gguf     # the file name Unsloth wrote
   ndcart ai serve --model local/model-q4_k_m.gguf   # the path under the models folder that import printed
   ndcart ai test
   ndcart ai eval --repeat 3
   ```

   Compare the eval report with the base model's before trusting the new one; a fine-tune can
   make a model worse.
