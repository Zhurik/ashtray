# ashtray

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

## Hooks

Formatting and lints run through [pre-commit](https://pre-commit.com). Wire
them into the clone once:

```sh
pre-commit install --install-hooks
```

That installs both hook types the config asks for. On commit: whitespace and
file hygiene, `typos`, `cargo fmt --check`, and `cargo clippy --all-targets
-D warnings`. On push: `cargo test`, which is the slow one and does not belong
in the way of a commit.

Run everything by hand without committing:

```sh
pre-commit run --all-files                        # commit-stage hooks
pre-commit run --all-files --hook-stage pre-push  # those plus the tests
```

`git commit --no-verify` skips them when a work-in-progress commit has to
land anyway.

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
