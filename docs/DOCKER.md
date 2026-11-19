# Running nd-cartographer in Docker (TrueNAS SCALE and others)

The image serves the whole studio (editor, table view, HTTP API) on port
**8300**. Everything it saves lives in the `/data` volume: your maps, custom
assets and the access token.

## TrueNAS SCALE (24.10 and later, Apps)

1. **Storage**: create a dataset, for example `tank/apps/ndcart`, owned by the
   `apps` user (uid/gid 568), or note the owner's uid.
2. **Apps → Discover Apps → Custom App → Install via YAML** and paste
   [`docker-compose.yml`](../docker-compose.yml). Point the volume at the dataset:

   ```yaml
       volumes:
         - /mnt/tank/apps/ndcart:/data
   ```
   If the dataset owner is not 568, set `user: "<uid>:<gid>"` to match.
3. **Token**: set `ND_CARTO_TOKEN` to a long random string. Without it, one is
   generated on first start, saved in `/data/token` and printed once in the logs.
4. Start the app and open `http://<truenas-ip>:8300/?token=<token>` once; the
   browser keeps a cookie after that. For a TV or tablet do **not** use that token:
   turn on "share to table" in the studio and open the link it shows
   (`http://<truenas-ip>:8300/table/<project>?token=<table-token>`). The table token
   is scoped to that one project's table screen (no GM data, no editing) and stops
   working when sharing is turned off.

Older TrueNAS (Kubernetes apps): *Apps → Launch Docker Image* with image
`ghcr.io/8bit-boom/nd-cartographer:latest`, port 8300, a host-path volume at
`/data`, and the environment variables from the compose file.

## Linked to nd-world

If nd-world runs on the same machine, put both on one Docker network and set
`ND_CARTO_SERVER`, `ND_CARTO_EMAIL` and `ND_CARTO_PASSWORD_FILE` (a mounted
secret file; never put the password in the YAML). Publish, Open published, the
entity list and the AI features then work from the container.
`docker exec nd-cartographer ndcart doctor` checks the link step by step.

## Plain Docker

```bash
docker run -d --name nd-cartographer -p 8300:8300 \
  -e ND_CARTO_TOKEN="$(openssl rand -hex 24)" \
  -v "$PWD/data:/data" --user 568:568 \
  ghcr.io/8bit-boom/nd-cartographer:latest
```

Build it yourself: `docker build -t nd-cartographer .`

## A model on the same machine (optional): the `ai-cpu` and `ai-gpu` profiles

The compose file also holds two opt-in services that run a model with upstream
llama.cpp's `llama-server` (the image `ghcr.io/ggml-org/llama.cpp`). Nothing starts
unless you pick a profile; plain `docker compose up -d` still runs only `ndcart`.

1. **Download the model** with the studio image. It shows the size, checks the free disk,
   verifies the file's SHA-256 against the Hugging Face Hub, and resumes if interrupted.
   Only the model file is downloaded, never a program:

   ```bash
   docker compose run --rm ndcart ndcart ai models --data-dir /data     # what fits, sizes
   docker compose run --rm ndcart ndcart ai pull qwen3.5-4b --data-dir /data --yes
   docker compose run --rm ndcart ndcart ai key --data-dir /data        # makes /data/ai-runtime.key
   ```
   (The container needs outbound access to `huggingface.co` for the download. The model
   names in the catalog were written without live Hub access: if `ai pull` says a file is
   not in the repository it lists what is, and `ndcart ai pull unsloth/Repo-GGUF:File.gguf`
   fetches an exact file.) `ai pull` prints the file's path. The compose file expects
   `/data/models/unsloth/Qwen3.5-4B-GGUF/Qwen3.5-4B-UD-Q4_K_XL.gguf`; if yours differs, edit
   the `-m` line of the service.
2. **Start it**: `docker compose --profile ai-cpu up -d`, or for an NVIDIA card
   `docker compose --profile ai-gpu up -d`.
3. **Point the studio at it**: uncomment the three `ND_CARTO_LLM_*` lines in the `ndcart`
   service (`llm-gpu` instead of `llm` in the address for the GPU profile) and
   `docker compose up -d`. The key is read from `/data/ai-runtime.key`; it is never written
   in the YAML. Then check it: `docker compose exec ndcart ndcart ai test` (a verdict of
   good, usable or too weak) and `docker compose exec ndcart ndcart doctor`.

The model server publishes no port (only the studio, on the compose network, reaches it),
asks for the key on every request, and mounts `/data` read-only. Pin the image to a tag you
have tested: `:server` moves with every release. The CPU profile needs the RAM the model
needs (about 3-4 GB for the 4B model).

**NVIDIA on plain Docker** needs the NVIDIA Container Toolkit; `-ngl all` offloads every
layer to the card. For AMD or Vulkan use the images `:server-rocm` or `:server-vulkan`
instead and pass the devices they document (`/dev/kfd`, `/dev/dri`); see llama.cpp's
`docs/docker.md`. This was not tried here.

**TrueNAS SCALE:**

- For the GPU profile, turn on *Apps → Settings → Install NVIDIA Drivers* (it appears when
  an NVIDIA GPU is present) and reboot the app service as it asks.
- Paste the YAML as a Custom App as above. On 25.10 forum reports say the custom YAML
  needs a top-level `services:` key (it has one); this is unverified here.
- Both containers share one dataset, mounted at `/data`. The studio runs as uid 568 and owns
  it; the llama.cpp image only reads `/data/models` and the key, which is why the mount is
  read-only. The key file is private (mode 600, owner 568): the llama.cpp image runs as root by
  default and can read it. If you set `user:` on the model service, give that user read
  access to the key file and nobody else.
- Not tested on a real TrueNAS machine or GPU: CI runs `docker compose config` for each
  profile (no images are pulled), and the container's behaviour is the same `llama-server`
  the `ndcart ai serve` command manages on a desktop.

Without Docker, `ndcart ai setup` does all of this on one computer (see `ndcart ai --help`). The full guide is [LOCAL_AI.md](LOCAL_AI.md); scheduled drafting from a container is in [AI_RECIPES.md](AI_RECIPES.md#docker-manual).

## Notes

- Do not expose the port to the internet. Use a VPN, or a reverse proxy with
  TLS; the token is the only gate.
- Back up the dataset (a TrueNAS snapshot is enough: saving is append-only and
  survives a hard stop; a power cut can lose the last second of edits).
- The container health check is `ndcart healthcheck`.
- Not yet verified: the image has not been built or run on a real TrueNAS
  machine (the development sandbox has no Docker daemon). The CI job
  `docker` builds it and runs the health check on every push.

`ND_CARTO_ALLOWED_HOSTS` (comma-separated host names) only matters when the server listens on loopback behind a proxy; the container listens on all interfaces and uses the token instead, so no host allow-list applies there.
