# picrew-agent

Listen agent for [PiCrew](https://github.com/aedan/picrew). OpenCode runs on this box; the hub **dials in**. This process does not phone home.

## Run

```bash
PICREW_TOKEN=secret picrew-agent --host 0.0.0.0 --port 5280 \
  --name builder-1 \
  --opencode http://127.0.0.1:4096 \
  --projects /srv/projects
```

| Flag / env | Default | Meaning |
|---|---|---|
| `--host` | `0.0.0.0` | Bind address |
| `--port` / `PICREW_AGENT_PORT` | `5280` | Port the hub dials |
| `--token` / `PICREW_TOKEN` | required | Shared secret |
| `--name` / `PICREW_AGENT_NAME` | hostname | Name reported to the hub |
| `--opencode` / `OPENCODE_URL` | `http://127.0.0.1:4096` | Local OpenCode |
| `--projects` / `PICREW_PROJECTS` | `/srv/projects` | Repo parent directory |

Protocol: `GET /healthz`, `GET /info`, `POST /rpc` with `X-PiCrew-Token` (or `Authorization: Bearer`). RPC kinds: `opencode`, `exec`, `config`.

## Releases

GitHub Actions builds a static **linux x86_64 musl** binary on every tag `v*`.

```
https://github.com/aedan/picrew-agent/releases/latest/download/picrew-agent-x86_64-unknown-linux-musl
```

PiCrew cloud-init pulls that file onto new VMs.

Tag a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Develop

```bash
cargo test
cargo build --release
```
