# AGENTS.md

Instructions for anyone (human or agent) changing this repository. Read
[codebase.md](codebase.md) for the architecture and request flows; this file is about
how to work here without breaking things.

## What this project is

`nujijushi` is a single Rust binary: it imports artwork (gallery-dl sources, local
folders, Telegram posts) into a content-addressed archive with SQLite metadata, curates
it, mirrors it to external storage, and publishes it to Telegram channels on a schedule.
Three interfaces sit on top of the same services: **CLI**, **axum admin UI**, and a
**Telegram bot** (long polling or webhook). 270 tests, no network needed to run them.

## Build, test, lint

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                       # hermetic: network/tool calls are mocked
cargo build --release            # rustls + bundled sqlite, no system openssl
```

CI (`.github/workflows/ci.yml`) runs exactly `cargo fmt --all --check`,
`cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked` on
Ubuntu **and** macOS. A change is not done until all three pass locally.

Toolchain: Rust **1.95**, edition **2024** (`rust-toolchain.toml` pins it). Do not raise
the MSRV or add a feature that needs nightly.

## Invariants — breaking one of these is a bug

1. **Publishing is idempotent.** Every send goes through
   `queue::lease::acquire` (`src/queue/lease.rs`) before touching Telegram. A `sending`
   row older than `STALE_LEASE_SECS` (600 s) becomes `unknown` and is **never** re-sent
   automatically; partial deliveries (some messages of a post out, then a failure) also
   become `unknown`. If you change the publish path, keep those two properties.
2. **Telegram media rules.** ≤ `MAX_ALBUM` (10) items per album, videos never share a
   group with photos, exactly **one** captioned item per group, caption ≤ `CAPTION_LIMIT`
   (1024) characters with tags closed when truncated. All of this lives in
   `src/media/grouping.rs` and `src/render/truncate.rs` — reuse it, don't re-derive it.
3. **One NSFW decision point.** `nsfw::evaluate` is the only thing that decides whether a
   post may go to a channel (hard-blocked tags → policy → spoiler). It runs at enqueue
   time *and* again at send time. Never publish on a different code path.
4. **Access control order** (`src/bot/access.rs`): config admins → DB role → config
   guests → `public` auto-register → refuse; `banned` always wins. Publishing from
   Telegram is admin-only *and* opt-in (`[access] allow_publish`, default false).
5. **SQL lives only in `src/db/`.** Other modules get typed functions. From async code use
   `Db::call` (spawn_blocking); in CLI paths and tests `Db::with` is fine.
6. **Storage layout is a public contract**: `<root>/<source>/<artist>/<source_id>/NN.ext`
   plus `post.json`. Changing it silently means re-uploading or orphaning archives.
7. **Network egress goes through the shared client.** Use `crate::http::client(cfg)` (it
   carries `[proxy]`) or the `Transport` trait. External tools get `--proxy` / `-x` via
   `http::proxy_args`. Never build a bare `reqwest::Client` or a raw `Command` for HTTP.
8. **Secrets never get logged**: bot token, UI token, webhook secret. `config.toml` is
   written 0600. The webhook endpoint authenticates with
   `auth::constant_time_eq` on the secret header, not the UI cookie.
9. **Every web POST carries a CSRF field** checked with `check_csrf`; templates
   autoescape, and captions are the only `| safe` values because the pipeline escaped
   them already.

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
* **Commits**: conventional commits (`feat:`, `fix:`, `ci:`, `docs:`), one logical change,
  body explains *why*.

## Adding things (checklists)

| changing | also do |
|---|---|
| schema | new `migrations/000N_*.sql`, register it in `src/db/migrate.rs`, add the table to that file's test, never edit an applied migration |
| config key | `config.example.toml`, the starter config in `src/config.rs`, `README.md` config block, `.env.example` if env-supported, a `doctor` line, and a parse test |
| CLI command | `src/cli.rs` enum + handler, a case in `mod parse_tests`, the README command table |
| web page/form | `src/web/routes.rs`, a template in `templates/`, register it in `src/web/views.rs`, CSRF on POSTs, and a render test |
| bot command | `src/bot/commands.rs` `MENU` + parse test, `src/bot/handlers.rs`, permission via `Perm` in `src/bot/access.rs`, help text |
| storage sink | implement `storage::Sink`, add its config section, wire it in `storage::sinks`, test it against a real local server if it speaks HTTP |
| caption feature | `src/render/caption.rs` context/filters + an `insta` snapshot in `tests/captions.rs` |
| source/importer | `src/source/`, return `NormalizedPost`, route it in `source::fetch_target`, test the metadata mapping with a JSON fixture |

## Testing conventions

* Unit tests live next to the code; cross-module flows go in `tests/`.
* `tests/flow.rs` — import → queue → publish against `tg::mock::MockTransport`
  (albums, spoilers, policy vetoes, transient vs permanent errors, partial delivery,
  stale leases, on-demand `publish_item`).
* `tests/bot.rs` — access control, commands, discovery, consent-free Telegram ingest.
* `tests/webhook.rs` — the HTTP endpoint via `tower::oneshot` (secret handling, auth
  bypass, malformed bodies).
* `tests/storage.rs` — sinks against a real local axum WebDAV server, a fake HTTP proxy
  proving uploads are proxied, mirror bookkeeping, backfill.
* `tests/captions.rs` + `tests/snapshots/` — the exact text sent to Telegram.
  Regenerate only with `INSTA_UPDATE=always cargo test --test captions`, then read the diff.
* Use `tempfile::tempdir()` for state and `Config::load_from_home` to build configs; never
  touch `$HOME`, never hit the network, never require `gallery-dl`/`ffmpeg`.

## Manual verification before calling something finished

```bash
export NUJIJUSHI_HOME=$(mktemp -d) && ./target/debug/nujijushi init
./target/debug/nujijushi doctor --offline      # config, schema, access, storage, proxy
./target/debug/nujijushi import <folder|url>   # then `ls`, `show <id>`
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
