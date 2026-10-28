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
message 800 characters. Characters, not bytes. Anything outside them comes
back as `422` with `{"error": "invalid_entry", "message": "…"}`; a body that
is not JSON comes back as `400` in the same shape.

### `GET /api/entries`

One page of entries, newest first. `page` starts at 1, `per_page` defaults to
20 and is capped at 100. Both are clamped rather than rejected, so walking off
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
runs a real query, so it fails when the disk or the file does.

## Running it

```sh
cargo run
```

The database file is created on first start and migrations run themselves.

| Variable | Default | What it does |
| --- | --- | --- |
| `ASHTRAY_BIND_ADDR` | `127.0.0.1:8080` | Address to listen on |
| `ASHTRAY_DATABASE_URL` | `sqlite://ashtray.db` | SQLite file, `sqlite::memory:` for a throwaway one |
| `ASHTRAY_MAX_CONNECTIONS` | `5` | Pool size |
| `ASHTRAY_LOG` | `info` | `tracing` filter, e.g. `ashtray=debug,tower_http=debug` |

```sh
cargo test
```

### In Docker

```sh
docker compose up --build
```

The image listens on `0.0.0.0:8080` and keeps the database in `/data`, which
`compose.yaml` mounts as a named volume so the entries outlive the container.
`compose.yaml` spells out every variable with its default; override any of them
from the shell or a `.env` file, and move the published port with
`ASHTRAY_PORT`.
`HEALTHCHECK` polls `/health`, the process runs as an unprivileged user, and
`SIGTERM` from `docker stop` goes through the graceful shutdown.

Without Compose:

```sh
docker build -t ashtray .
docker run -p 8080:8080 -v ashtray-data:/data ashtray
```

Every variable from the table above still works, e.g.
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
a `linux/amd64` + `linux/arm64` image tagged `latest`, `X.Y.Z`, `X.Y` and `X`,
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
- **No spam protection.** No rate limit, no honeypot, no captcha. An endpoint
  open to the internet in this state will be full of casino links within a
  week; put a rate limit in front of it, at the very least.
- **No CORS.** A browser can post to it with a plain HTML form, but JavaScript
  on another origin cannot.
- **No deletion or editing.** Entries only go in. Removing one means a `DELETE`
  against the database by hand.
