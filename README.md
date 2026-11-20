# nujijushi

[![ci](https://github.com/ZZU1U/nujijushi/actions/workflows/ci.yml/badge.svg)](https://github.com/ZZU1U/nujijushi/actions/workflows/ci.yml)

nujijushi is a local artwork archive that publishes curated posts to Telegram channels on a schedule. It runs as a single Rust binary with a CLI, a web admin UI, and a Telegram bot.

- **Import** artwork from sites like Pixiv, X, and Danbooru via `gallery-dl`, from local folders, or from Telegram posts.
- **Curate** a searchable SQLite archive with tags, ratings, thumbnails, and deduplicated media files.
- **Publish** albums with captions built from artist and source metadata, per-channel schedules, and NSFW rules.
- **Mirror** the archive to a local folder, WebDAV server, or private Telegram channel.

## Install

Download and extract the binary for your Linux or macOS architecture from [GitHub Releases](https://github.com/ZZU1U/nujijushi/releases), then put it on your `PATH`.

To install from a checkout with Rust 1.95:

```bash
cargo install --path . --locked
```

Install `gallery-dl` for gallery-site imports and `ffmpeg` if you want video thumbnails. Importing a folder of images only needs nujijushi.

## Quick start

For guided web setup, run `nujijushi init`, then `nujijushi run` and open the [admin UI](http://127.0.0.1:8788/). The dashboard walks you through configuring Telegram, importing artwork, and holding the first post for preview. Confirm the preview when you are ready to let the scheduler publish it.

1. **Initialize your archive.**

   ```bash
   nujijushi init
   ```

   Save the admin UI token printed by `init`. The default data directory is `~/.nujijushi`. Set `NUJIJUSHI_HOME` before starting if you want to use another directory.

2. **Connect a Telegram channel.** Create a bot with @BotFather and set `[telegram] bot_token` in the generated `config.toml`, or set the `TG_BOT_TOKEN` environment variable. Give the bot permission to post as an administrator of the target channel.

   ```bash
   nujijushi channel add art @yourchannel --interval 180 --nsfw spoil
   ```

   Use its numeric `-100…` ID for a private channel. If you plan to manage the archive from Telegram, add your user ID to `[access] admins`. You can find it with `/whoami` in a private chat with the bot once it’s running.

3. **Import, review, and queue artwork.**

   ```bash
   nujijushi import ./artwork
   nujijushi ls --state new
   nujijushi show 1 --channel art
   nujijushi queue add 1 --channel art
   ```

   Replace `./artwork` with your folder or a supported artwork URL, and `1` with the post ID from `ls`.

4. **Preview the queue and inspect a dry run.**

   ```bash
   nujijushi preview art
   nujijushi push --channel art --dry-run
   ```

5. **Start scheduled publishing.**

   ```bash
   nujijushi run
   ```

   Open the [admin UI](http://127.0.0.1:8788/) with the token from step 1. Use `nujijushi serve` when you want the UI and its background jobs without scheduled sends.

   Set `[bot] queue_alerts = true` to DM the Telegram users in `[access] admins` about empty queues and unconfirmed sends. Each recipient must start a private chat with the bot first.
   Set `[bot] guest_reputation = true` to score guest suggestions and show their balance in `/whoami`.

## Optional artwork tag suggestions

Set up [FoidFinder](contrib/foidfinder/README.md) on the machine running `nujijushi`. Haruka can run on CPU through ONNX Runtime. Add this section to `config.toml` and restart `nujijushi`.

```toml
[tag_suggestions]
enabled = true
tagger_port = 8790
online_fallback = false
```

Open a post and choose **Suggest tags**. Review the character and franchise suggestions, then save or discard each one. FoidFinder runs locally and supplies both character and series tags. The review fills to three characters when enough eligible guesses are available and labels guesses below the model's threshold **Low confidence**.

The optional online fallback checks local mappings and the cache, then queries Danbooru using the names or known IDs of up to three unresolved strong character matches. It verifies a single franchise from the character wiki's introduction. Images stay on the machine. Successful matches can be reused offline, and definitive misses are cached for seven days.

## Documentation

| Guide | Covers |
|---|---|
| [Usage reference](docs/usage.md) | CLI commands, channel discovery, scheduling, NSFW policy, captions, and the admin UI |
| [Operations guide](docs/operations.md) | Configuration, cookies, bot commands and roles, webhooks, proxies, mirrors, and troubleshooting |
| [Example config](config.example.toml) | Core settings with comments and defaults |
| [Browser extension](extension/README.md) | Clip artwork URLs for batch import |
| [Architecture](codebase.md) · [Contributing](AGENTS.md) | Request flows, invariants, and development checks |

For setup problems, start with `nujijushi doctor`. Use `nujijushi doctor --offline` to check local configuration and dependencies.

Archive and republish artwork you have the right to share.
