# AGENTS.md

Instructions for anyone (human or agent) changing this repository. Read
[codebase.md](codebase.md) for the architecture and request flows; this file is about
how to work here without breaking things.

## What this project is

`nujijushi` is a single Rust binary: it imports artwork (gallery-dl sources, local
folders, Telegram posts) into a content-addressed archive with SQLite metadata, curates
it, mirrors it to external storage, and publishes it to Telegram channels on a schedule.
Three interfaces sit on top of the same services: **CLI**, **axum admin UI**, and a
**Telegram bot** (long polling or webhook). Tests use mocked external services and
tools. HTTP integration tests bind loopback sockets.

## Build, test, lint

```bash
npm ci --prefix webui
npm run build --prefix webui
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

CI (`.github/workflows/ci.yml`) runs formatting and Clippy on Ubuntu, Rust tests
on Ubuntu and macOS, and Playwright on Ubuntu. Build the frontend before Clippy
and the Rust tests to match CI. A change is done when all three Rust checks pass
locally. For admin UI changes, also run the browser suite after building the bundle.

```bash
NUJIJUSHI_REQUIRE_WEBUI=1 cargo build --locked
npx --prefix webui playwright install chromium
npm test --prefix webui
```

Toolchain: Rust **1.95**, edition **2024** (`rust-toolchain.toml` pins it). Do not raise
the MSRV or add a feature that needs nightly. CI uses Node.js **24** for the admin UI.

## Invariants — breaking one of these is a bug

1. **Publishing is idempotent.** Every publish takes a queue lease through
   `queue::lease::acquire_for_gallery` (`src/queue/lease.rs`), which also checks the
   outgoing gallery revision before touching Telegram. A `sending`
   row older than `STALE_LEASE_SECS` (600 s) becomes `unknown` and is **never** re-sent
   automatically; partial deliveries (some messages of a post out, then a failure) also
   become `unknown`. If you change the publish path, keep those two properties.
   **The web never sends to Telegram inside a request**: `POST /api/v1/publish` and
   `POST /api/v1/storage/drain` only create a job (`jobs::create_*_job`, deduplicated by
   `db::jobs::find_active`) and the worker runs it.
2. **Telegram publishing media rules.** ≤ `MAX_ALBUM` (10) items per album, videos
   send individually, and the post caption goes on the first media item. Captions
   fit within `CAPTION_LIMIT` (1024) characters and tags close when truncated. Reuse
   `src/media/grouping.rs` and `src/render/truncate.rs`.
3. **One NSFW decision point.** `nsfw::evaluate` is the only thing that decides whether a
   post may go to a channel (hard-blocked tags → policy → spoiler). It runs at enqueue
   time *and* again at send time. Never publish on a different code path.
4. **Access control order** (`src/bot/access.rs`): config admins → DB role → config
   guests → `public` auto-register → refuse. A config admin is always an admin (the stored
   row may still say `guest`: messaging auto-registers one); for everyone else `banned`
   beats guest and `public`. Listings print the effective role via
   `bot::access::display_role`, never the raw row. Publishing from Telegram is admin-only
   *and* opt-in (`[access] allow_publish`, default false).
5. **SQL lives only in `src/db/`.** Other modules get typed functions. From async code use
   `Db::call` (spawn_blocking); in CLI paths and tests `Db::with` is fine.
6. **Storage layout is a public contract.** Filesystem and WebDAV mirrors write
   `<root>/<source>/<artist>/<source_id>/NN.ext` plus `post.json`. Share exports
   use the same per-post layout. Changing it silently means re-uploading or orphaning archives.
7. **Network egress goes through the shared client.** Use `crate::http::client(cfg)` (it
   carries `[proxy]`) or the `Transport` trait. External tools get `--proxy` / `-x` via
   `http::proxy_args`. Never build a bare `reqwest::Client` or a raw `Command` for HTTP.
8. **Secrets never get logged**: bot token, UI token, webhook secret. `config.toml` is
   written 0600. The webhook endpoint authenticates with
   `auth::constant_time_eq` on the secret header, not the UI cookie.
9. **Every admin POST carries a CSRF field.** Sign-in checks its cookie-bound
   challenge and authenticated actions use `check_csrf`. React escapes text and the
   caption preview component allowlists Telegram formatting.

## Code conventions

* **Layering**: `model` ← `db`/`blob` ← feature modules (`source`, `render`, `nsfw`,
  `media`, `queue`, `tg`, `storage`) ← interfaces (`cli`, `web`, `bot`). Services never
  import an interface. `tg` is the only module that knows about teloxide.
* **Errors**: `crate::error::Error` with `Result<T>` everywhere. The Telegram transport
  uses its own `TgResult<T> = Result<_, TgError>` so callers must map/classify
  (`TgError::classify` handles `retry after`, permanent chat errors, transient blips).
* **DB-backed enums**: use the `string_enum!` macro in `src/model.rs` (gives `as_str`,
  lenient `parse`, strict `try_parse`, `Display`, `FromStr`).
* **Async**: futures are spawned, so they must be `Send`. Do not hold `rand::rng()`,
  `RefCell` guards or `Db::with` locks across an `.await` — this already caused one bug.
* **Files**: one concern per file, tests at the bottom of the same file under
  `#[cfg(test)] mod tests`. Prefer extracting a pure function over mocking.
* **Commits**: conventional commits, one logical change each; see
  [Commit style and consistency](#commit-style-and-consistency).

## Commit style and consistency

* **Format**: `<type>(<scope>): <imperative summary>`, e.g.
  `feat(web): resolve stale sends from the queue`. Use a lowercase type and scope, an
  imperative summary without a trailing period, and keep the subject under ~72
  characters. Reuse the existing vocabulary: `feat`, `fix`, `perf`, `refactor`, `test`,
  `docs`, `ci`, `style`, `chore`; scopes are module or area names such as `web`, `bot`,
  `tg`, `queue`, `storage`, `import`, `db`, `config`, `cli`, `docs`. Check `git log`
  before inventing a new one.
* **One concern per commit**: a commit must be understandable and revertible on its own.
  Do not mix unrelated fixes, formatting sweeps, dependency bumps, or documentation for
  a different feature. Split by concern, not by file; when one file carries several
  changes, stage individual hunks (`git add -p`) rather than committing the whole file.
  A feature's required config, migration, tests and docs belong with that feature (or a
  clearly named follow-up), not in a separate random commit.
* **Body**: explain *why* the change exists — the bug or limitation, the chosen
  trade-off, and any behavior or migration note. Reference an invariant, command, or
  test when it makes the reason concrete. The subject says what changed; the body says
  why it had to.
* **Keep each commit working**: run `cargo fmt --all --check`, `cargo clippy ...` and the
  relevant tests before the final commit; when a change spans many files, at least run
  `cargo check --workspace --all-targets --locked` at each intermediate commit. Do not
  leave a commit that only compiles because the next one adds a missing symbol.
* **Consistency**: match the surrounding style in code and UI copy — same label casing
  (sentence case for headings and actions), whitespace and spacing conventions, icon or
  badge usage, and naming. If a commit changes user-visible text, update the tests that
  assert it (`tests/web.rs`, `tests/bot.rs`, `webui/tests/admin.spec.ts`) in the same
  commit. Commit messages use the same type/scope vocabulary throughout the series.
* **Review your split before pushing**: `git log --oneline` should read as a sequence of
  small, coherent steps; `git show --stat <commit>` should match its subject. Do not
  rewrite shared history just to be tidy — squash or fix up only unpushed commits.
* **Never commit** generated output (`target/`, `webui/dist`, Playwright
  `test-results/`, build artifacts), secrets, a real `config.toml`, cookies, or a data
  directory. `webui/dist` is a build input embedded by `build.rs`; it is not source.

## Adding things (checklists)

| changing | also do |
|---|---|
| schema | new `migrations/000N_*.sql`, register it in `src/db/migrate.rs`, add the table to that file's test, never edit an applied migration |
| config key | `config.example.toml`, the starter config in `src/config.rs`, `README.md` config block, `.env.example` if env-supported, a `doctor` line, and a parse test |
| CLI command | `src/cli.rs` enum + handler, a case in `mod parse_tests`, the README command table |
| web page/form | register the JSON API in `src/web/routes.rs`, add the React screen and typed API data in `webui/src/`, check CSRF on writes, and test the API and rendered workflow |
| slow web action | a job kind in `src/jobs/mod.rs` (`run_kind`), a `create_*_job` constructor, an accepted response with its job ID, the shared job view for status and React, and a `tests/web.rs` case |
| bot command | `src/bot/commands.rs` `MENU` + parse test, `src/bot/handlers.rs`, permission via `Perm` in `src/bot/access.rs`, help text |
| storage sink | implement `storage::Sink`, add its config section, wire it in `storage::sinks`, test it against a real local server if it speaks HTTP |
| caption feature | `src/render/caption.rs` context/filters + an `insta` snapshot in `tests/captions.rs` |
| source/importer | `src/source/`, return `NormalizedPost`, route it in `source::fetch_target`, test the metadata mapping with a JSON fixture (author fields are source-specific — the twitter/pixiv disagreement is spelled out in `codebase.md`, flow 1) |

## Testing conventions

* Unit tests live next to the code; cross-module flows go in `tests/`.
* `tests/flow.rs` — import → queue → publish against `tg::mock::MockTransport`
  (albums, spoilers, policy vetoes, transient vs permanent errors, partial delivery,
  stale leases, on-demand `publish_item`).
* `tests/bot.rs` — access control, commands, discovery, consent-free Telegram ingest.
* `tests/web.rs` and `webui/tests/admin.spec.ts` — the admin JSON API and browser workflows.
* `tests/webhook.rs` — the HTTP endpoint via `tower::oneshot` (secret handling, auth
  bypass, malformed bodies).
* `tests/storage.rs` — sinks against a real local axum WebDAV server, a fake HTTP proxy
  proving uploads are proxied, mirror bookkeeping, backfill.
* `tests/captions.rs` + `tests/snapshots/` — the exact text sent to Telegram.
  Regenerate only with `INSTA_UPDATE=always cargo test --test captions`, then read the diff.
* Use `tempfile::tempdir()` for state and `Config::load_from_home` to build configs.
  Keep tests off `$HOME` and external services. Use loopback for local HTTP fixtures
  and fake `gallery-dl`/`ffmpeg` when needed.

## Manual verification before calling something finished

```bash
export NUJIJUSHI_HOME=$(mktemp -d) && ./target/debug/nujijushi init
./target/debug/nujijushi doctor --offline      # config, schema, access, storage, proxy
./target/debug/nujijushi import <folder|url>   # then `ls --state approved`, `show <id>`
./target/debug/nujijushi preview <channel>
./target/debug/nujijushi push --channel <alias> --dry-run   # prints the exact payload
./target/debug/nujijushi storage test && ./target/debug/nujijushi storage sync --dry-run
```

Live Telegram (publishing, bot commands, `/publish`) needs a real `TG_BOT_TOKEN` and the
bot as a channel admin — that is the one part tests cannot cover; use `--dry-run` first.

## Do not

* rename or move `migrations/*` or change the storage layout without a migration/migration path;
* call Telegram outside `src/tg/` or SQL outside `src/db/`;
* add a dependency for something 50 lines of std/`reqwest` can do (current graph is
  deliberate: rustls instead of openssl, `rusqlite` bundled, no async DB layer);
* leave `unwrap()`/`expect()` in request paths — only in tests and on checked invariants;
* commit `target/`, `.pi/`, a real `config.toml`, tokens, cookies, or a data directory.
