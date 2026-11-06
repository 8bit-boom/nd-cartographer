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
   browser keeps a cookie after that. For a TV or tablet use
   `http://<truenas-ip>:8300/table/<project>?token=<token>`.

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

## Notes

- Do not expose the port to the internet. Use a VPN, or a reverse proxy with
  TLS; the token is the only gate.
- Back up the dataset (a TrueNAS snapshot is enough: saving is append-only and
  survives a hard stop; a power cut can lose the last second of edits).
- The container health check is `ndcart healthcheck`.
- Not yet verified: the image has not been built or run on a real TrueNAS
  machine (the development sandbox has no Docker daemon). The CI job
  `docker` builds it and runs the health check on every push.
