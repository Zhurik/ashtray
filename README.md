# ashtray

[![CI](https://github.com/Zhurik/ashtray/actions/workflows/ci.yaml/badge.svg)](https://github.com/Zhurik/ashtray/actions/workflows/ci.yaml)
[![Release](https://github.com/Zhurik/ashtray/actions/workflows/release.yaml/badge.svg)](https://github.com/Zhurik/ashtray/actions/workflows/release.yaml)
[![GitHub release](https://img.shields.io/github/v/release/Zhurik/ashtray)](https://github.com/Zhurik/ashtray/releases)
[![semantic-release](https://img.shields.io/badge/semantic--release-e10079?logo=semantic-release&logoColor=white)](https://github.com/semantic-release/semantic-release)
[![Conventional Commits](https://img.shields.io/badge/Conventional%20Commits-1.0.0-fe5196?logo=conventionalcommits&logoColor=white)](https://conventionalcommits.org)
[![pre-commit](https://img.shields.io/badge/pre--commit-enabled-FAB040?logo=pre-commit&logoColor=black)](https://pre-commit.com)

[![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Axum](https://img.shields.io/badge/Axum-0.8-6E4A7E)](https://github.com/tokio-rs/axum)
[![Tokio](https://img.shields.io/badge/Tokio-async-463E3E)](https://tokio.rs)
[![SQLite](https://img.shields.io/badge/SQLite-003B57?logo=sqlite&logoColor=white)](https://www.sqlite.org)
[![Docker](https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=white)](./Dockerfile)
[![Clippy](https://img.shields.io/badge/clippy--D%20warnings-approved-B7410E?logo=rust)](https://github.com/rust-lang/rust-clippy)
[![typos](https://img.shields.io/badge/typos-none%20(hopefully)-blueviolet)](https://github.com/crate-ci/typos)

[![Smoking room](https://img.shields.io/badge/smoking%20room-open-4C4C4C)](#ashtray)
[![Endpoints](https://img.shields.io/badge/endpoints-3-orange)](#endpoints)
[![Admin panel](https://img.shields.io/badge/admin%20panel-nope-red)](#what-it-deliberately-does-not-do-yet)
[![Casino links](https://img.shields.io/badge/casino%20links-incoming-yellow)](#what-it-deliberately-does-not-do-yet)

A guestbook backend. Everything said in the smoking room ends up here.

Three endpoints, one SQLite file, no users, no sessions, no admin panel. It
exists to back the guestbook page of a static site, so it does the smallest
thing that can work: take a note, hand the notes back, say whether it is alive.

## Endpoints

### `POST /api/entries`

Writes one entry. `body` is required; `author` and `url` are optional, and a
blank string counts as absent — an entry with no author is anonymous.

```sh
curl -X POST http://127.0.0.1:8080/api/entries \
  -H 'content-type: application/json' \
  -d '{"author": "marina", "url": "https://marina.dev", "body": "Hello from the webring."}'
```

```json
{
  "id": 1,
  "author": "marina",
  "url": "https://marina.dev",
  "body": "Hello from the webring.",
  "created_at": "2026-10-27T09:12:00Z"
}
```

Limits: author 40 characters, link 200 characters and `http`/`https` only,
message 800 characters. Characters, not bytes, and each of them can be moved
through the [environment](#configuration). Anything outside them comes back as
`422` with `{"error": "invalid_entry", "message": "…"}`; a body that is not
JSON comes back as `400` in the same shape.

The entry is stored exactly as written. The page that shows it has to escape
`author`, `url` and `body` as text — never insert them as HTML.

### `GET /api/entries`

One page of entries, newest first. `page` starts at 1, `per_page` defaults to
20 and is capped at 100 (`ASHTRAY_DEFAULT_PER_PAGE`, `ASHTRAY_MAX_PER_PAGE`). Both are clamped rather than rejected, so walking off
the end of the list returns an empty page, not an error.

```sh
curl 'http://127.0.0.1:8080/api/entries?page=1&per_page=20'
```

```json
{
  "items": [],
  "page": 1,
  "per_page": 20,
  "total": 0,
  "total_pages": 0
}
```

### `GET /health`

`200` with `{"status": "ok", "database": "up"}` while SQLite answers, `503`
with `{"status": "degraded", "database": "down"}` when it does not. The check
runs a real query, so it fails when the disk or the file does. It is never
rate limited, and it is logged at `debug` so that a monitor polling it does not
drown out the visitors.

### Errors

Every error, from any endpoint or from the layers in front of them, has the
same shape — the `error` code is stable and meant for code, the `message` is
meant for people:

```json
{"error": "rate_limited", "message": "Too many requests. Try again in 360 seconds."}
```

| Status | `error` | When |
| --- | --- | --- |
| `400` | `bad_request` | The body is not JSON, a field is missing, or the query string is malformed |
| `404` | `not_found` | Nothing is routed at that path |
| `405` | `method_not_allowed` | Wrong method; the `Allow` header lists the right ones |
| `413` | `payload_too_large` | The body is over `ASHTRAY_MAX_BODY_SIZE` |
| `415` | `unsupported_media_type` | The body is not sent as `application/json` |
| `422` | `invalid_entry` | The entry breaks one of the limits above |
| `429` | `rate_limited` | The client spent its budget; `Retry-After` says for how long, in seconds |
| `500` | `internal_error` | The database failed or a handler panicked — the details are in the log only |
| `503` | `timeout` | The request took longer than `ASHTRAY_REQUEST_TIMEOUT` |

Every response carries an `x-request-id` header. An incoming one (from a proxy,
say) is kept; otherwise a UUID is made up. The log line of the request carries
the same id, so a visitor's bug report can be matched to it.

## Running it

```sh
cargo run
```

The database file is created on first start and migrations run themselves.

```sh
cargo test
```

### Configuration

Everything is an environment variable; nothing needs a rebuild. An unset or
empty variable takes its default. A variable that is set but cannot be parsed
stops the process at startup with exit code `78`, listing every bad variable at
once rather than one per restart.

Durations are written like `10s`, `500ms` or `1m 30s`; sizes like `16KiB`,
`1MiB` or a plain number of bytes; rates as `<count>/<period>`, such as `10/1h`
or `120/min`, or `off`.

**Server**

| Variable | Default | What it does |
| --- | --- | --- |
| `ASHTRAY_BIND_ADDR` | `127.0.0.1:8080` | Address to listen on |
| `ASHTRAY_DATABASE_URL` | `sqlite://ashtray.db` | SQLite file, `sqlite::memory:` for a throwaway one |
| `ASHTRAY_MAX_CONNECTIONS` | `5` | Pool size |
| `ASHTRAY_HEADER_READ_TIMEOUT` | `10s` | How long a client gets to send its request headers; also closes idle keep-alive connections |
| `ASHTRAY_REQUEST_TIMEOUT` | `10s` | How long a request gets to be answered before it becomes a `503` |
| `ASHTRAY_SHUTDOWN_TIMEOUT` | `8s` | How long open requests get to finish after `SIGTERM`; keep it under the orchestrator's kill timeout |

**Logging**

| Variable | Default | What it does |
| --- | --- | --- |
| `ASHTRAY_LOG` | `info` | `tracing` filter, e.g. `ashtray=debug,tower_http=debug` |
| `ASHTRAY_LOG_FORMAT` | `text` | `text` for people, `json` (one object per line) for a log collector |

**Browsers and clients**

| Variable | Default | What it does |
| --- | --- | --- |
| `ASHTRAY_CORS_ALLOWED_ORIGINS` | *(empty: CORS off)* | Origins whose scripts may call the API: `https://example.com`, a comma-separated list, or `*` |
| `ASHTRAY_CORS_MAX_AGE` | `10m` | How long a browser may cache a preflight answer |
| `ASHTRAY_TRUSTED_PROXIES` | *(empty: none)* | Peers whose forwarding header is believed: addresses, networks (`10.0.0.0/8`), `loopback`, `private` |
| `ASHTRAY_CLIENT_IP_HEADER` | `x-forwarded-for` | Where those proxies put the client address; `x-real-ip`, `cf-connecting-ip` and the standard `forwarded` work too |

**Abuse**

| Variable | Default | What it does |
| --- | --- | --- |
| `ASHTRAY_RATE_LIMIT_POST` | `10/1h` | Entries one client may write: ten at once, then one more every six minutes |
| `ASHTRAY_RATE_LIMIT_GET` | `120/1m` | Pages one client may read |
| `ASHTRAY_MAX_BODY_SIZE` | `16KiB` | Largest request body accepted |

**Entries**

| Variable | Default | What it does |
| --- | --- | --- |
| `ASHTRAY_MAX_AUTHOR_CHARS` | `40` | Longest name |
| `ASHTRAY_MAX_URL_CHARS` | `200` | Longest link |
| `ASHTRAY_MAX_BODY_CHARS` | `800` | Longest message |
| `ASHTRAY_DEFAULT_PER_PAGE` | `20` | Page size when the client does not ask for one |
| `ASHTRAY_MAX_PER_PAGE` | `100` | Largest page size a client can ask for |

Settings that are valid but probably a mistake — a body limit too small for an
entry at the character limits, for instance — are logged as warnings at startup
and do not stop the process. The startup line itself lists the effective
settings.

The binary also answers `ashtray healthcheck` (exit `0` when the server on
`ASHTRAY_BIND_ADDR` answers `/health`), `ashtray version` and `ashtray help`.

### Running it in production

**CORS.** Set `ASHTRAY_CORS_ALLOWED_ORIGINS` to the origin of the site that
embeds the guestbook, exactly as the browser shows it: scheme, host and port,
no path. `https://example.com/` is normalized to `https://example.com`; a path
is refused at startup. `*` works but lets any page on the internet post on its
visitors' behalf.

**Behind a reverse proxy.** The rate limit counts per client address, and
behind a proxy every request comes from the proxy. Tell ashtray which peers
are proxies and it reads the client from their header — and only from theirs,
so nobody else can pick an address by sending `X-Forwarded-For` themselves.
`X-Forwarded-For` is read from the right: the first address that is not a
trusted proxy is the client, whatever the client wrote to the left of it.

With nginx in front:

```nginx
location /api/ {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Request-Id $request_id;
}
```

```sh
ASHTRAY_TRUSTED_PROXIES=loopback        # nginx on the same host
ASHTRAY_TRUSTED_PROXIES=private         # nginx in the same Docker network
ASHTRAY_TRUSTED_PROXIES=173.245.48.0/20,103.21.244.0/22,…  # Cloudflare's ranges
```

Behind Cloudflare alone, `ASHTRAY_CLIENT_IP_HEADER=cf-connecting-ip` with
Cloudflare's published ranges as the trusted proxies is the simpler setup.
With nothing in front at all, leave both unset: the TCP peer is the client.

**Rate limits** live in process memory. Each replica counts on its own and a
restart forgives everyone — fine for one instance, which is what SQLite allows
anyway. IPv6 clients are counted per `/64`, since that is what one subscriber
usually gets.

**Backups.** The database is one SQLite file in WAL mode. On a clean stop the
WAL is folded back into the main file, so copying `ashtray.db` out of a stopped
container's volume is a complete backup. For a live one, use SQLite's own
backup rather than `cp`, which can catch the file mid-write — from the host,
against the volume, since the runtime image ships no `sqlite3`:

```sh
sqlite3 /path/to/volume/ashtray.db ".backup ashtray-$(date +%F).db"
```

Or replicate continuously with [Litestream](https://litestream.io).



### Prebuilt image

Every release is published to a public registry, no login needed, for
`linux/amd64` and `linux/arm64`:

```sh
docker pull zhurik.cr.cloud.ru/ashtray:latest
docker run -d -p 8080:8080 -v ashtray-data:/data zhurik.cr.cloud.ru/ashtray:latest
```

Tags: `latest`, the exact version (`0.1.0`), and the moving `0.1` and `0`
lines. Pin one of the versioned tags anywhere that should not change under
you.

Nothing from this repository is needed to run it. A minimal `compose.yaml`:

```yaml
services:
  ashtray:
    image: zhurik.cr.cloud.ru/ashtray:0
    ports:
      - "8080:8080"
    environment:
      ASHTRAY_LOG: info
    volumes:
      - ashtray-data:/data
    restart: unless-stopped

volumes:
  ashtray-data:
```

The variables, volume and health check are the same as below.

### Building it yourself


```sh
docker compose up --build
```

The image listens on `0.0.0.0:8080` and keeps the database in `/data`, which
`compose.yaml` mounts as a named volume so the entries outlive the container.
`compose.yaml` spells out every variable with its default; override any of them
from the shell or a `.env` file, and move the published port with
`ASHTRAY_PORT`.
`HEALTHCHECK` runs `ashtray healthcheck` against `/health` — the runtime image
holds the binary and nothing else it would need `curl` for — the process runs
as an unprivileged user, and `SIGTERM` from `docker stop` goes through the
graceful shutdown.

Without Compose:

```sh
docker build -t ashtray .
docker run -p 8080:8080 -v ashtray-data:/data ashtray
```

Every variable from the tables above still works, e.g.
`docker run -e ASHTRAY_LOG=ashtray=debug …`.

## Hooks

Formatting and lints run through [pre-commit](https://pre-commit.com). The
commit-message hook runs `commitlint` from `node_modules`, so wire everything
into the clone once:

```sh
npm ci
pre-commit install --install-hooks
```

That installs every hook type the config asks for. On commit: whitespace and
file hygiene, `typos`, `yamllint`, `actionlint`, `hadolint`, `gitleaks`,
`cargo fmt --check`, `cargo clippy --all-targets -D warnings` and `cargo doc`
with warnings denied. On the commit message: `commitlint` against Conventional
Commits. On push: `cargo test`, which is the slow one and does not belong in
the way of a commit.

Run everything by hand without committing:

```sh
pre-commit run --all-files                        # commit-stage hooks
pre-commit run --all-files --hook-stage pre-push  # the tests
```

`git commit --no-verify` skips them when a work-in-progress commit has to
land anyway.

## CI and releases

`CI` runs on every pull request and every push to `main`: the commit-stage
pre-commit hooks, `cargo test`, `commitlint` over the pull request's commits,
and a `docker build`.

A green `CI` on `main` triggers `Release`, which runs
[semantic-release](https://github.com/semantic-release/semantic-release). It
reads the Conventional Commits since the last tag, picks the next version,
writes it into `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`, builds and pushes
a `linux/amd64` + `linux/arm64` image to `zhurik.cr.cloud.ru/ashtray` tagged `latest`, `X.Y.Z`, `X.Y` and `X`,
and publishes a GitHub release. `fix:` makes a patch, `feat:` a minor,
`BREAKING CHANGE:` a major; `chore:`, `docs:` and the rest release nothing.

The registry is configured in the repository settings, not in the code:

| Name | Kind | What it is |
| --- | --- | --- |
| `DOCKER_REGISTRY` | variable | Registry host; empty means Docker Hub |
| `DOCKER_PROJECT` | variable | Namespace in front of the image name, if any |
| `DOCKER_REGISTRY_USER` | secret | Registry login |
| `DOCKER_REGISTRY_PASSWORD` | secret | Registry password or token |

## What it deliberately does not do yet

- **No moderation.** Every entry is public the moment it is written. A
  `published` flag and a way to flip it is the obvious next step.
- **Only a rate limit against spam.** No honeypot, no captcha, no link
  filter. The limit slows a spammer down to `ASHTRAY_RATE_LIMIT_POST` per
  address; one with many addresses still gets through.
- **No shared rate limit.** The counts are per process, so running several
  replicas multiplies every budget. Redis or similar would fix that once there
  is more than one.
- **No deletion or editing.** Entries only go in. Removing one means a `DELETE`
  against the database by hand.
