//! Command line interface: thin handlers over the feature modules.

use std::path::PathBuf;

use chrono::{DateTime, Local, Utc};
use clap::{Args as ClapArgs, Parser, Subcommand};

use crate::bot::{queue_alerts, reputation_notifications};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{Attribution, Channel, ParseMode, PostState, QueueState, Rating, TagStyle};
use crate::state::AppState;
use crate::tg::{self, Transport};

#[derive(Debug, Parser)]
#[command(
    name = "nujijushi",
    version,
    about = "Local artwork archive with a scheduled Telegram channel publisher",
    long_about = None
)]
pub struct Args {
    /// Data directory (default: $NUJIJUSHI_HOME or ~/.nujijushi)
    #[arg(long, global = true, env = "NUJIJUSHI_HOME", value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// Increase log verbosity (-v debug, -vv trace)
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Debug, Subcommand)]
// a CLI arg enum: built once, so the variant size spread is irrelevant
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Create the data directory, config file and database
    Init {
        /// Bot token to store in config.toml (otherwise TG_BOT_TOKEN is used)
        #[arg(long, value_name = "TOKEN")]
        bot_token: Option<String>,
        /// Overwrite an existing config.toml
        #[arg(long)]
        force: bool,
    },

    /// Check the environment, config, database and Telegram access
    Doctor {
        /// Exit non-zero when a check fails
        #[arg(long)]
        strict: bool,
        /// Skip the Telegram API calls
        #[arg(long)]
        offline: bool,
    },

    /// Import artwork from URLs or local directories
    Import {
        /// URLs (pixiv, twitter/x, ...) or directories
        #[arg(required = true, value_name = "TARGET")]
        targets: Vec<String>,
        /// Also queue everything into this channel
        #[arg(long, value_name = "CHANNEL")]
        channel: Option<String>,
        /// Queue priority for imported posts
        #[arg(long, default_value_t = 0)]
        priority: i64,
    },

    /// List archived posts
    Ls {
        #[arg(long, value_name = "STATE")]
        state: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        /// Filter by nsfw flag (yes/no)
        #[arg(long)]
        nsfw: Option<String>,
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 40)]
        limit: i64,
    },

    /// Show one post in detail
    Show {
        id: i64,
        /// Render the caption for this channel
        #[arg(long, value_name = "CHANNEL")]
        channel: Option<String>,
        /// Dump the stored source metadata, exactly as the extractor emitted it
        #[arg(long)]
        raw: bool,
    },

    /// Add or remove tags
    Tag {
        id: i64,
        #[arg(value_name = "TAG")]
        tags: Vec<String>,
        #[arg(long)]
        remove: bool,
    },

    /// Set the content rating
    Rate {
        id: i64,
        #[arg(value_name = "sfw|questionable|explicit")]
        rating: String,
    },

    /// Override the nsfw flag (auto = derive from rating/tags)
    Nsfw {
        id: i64,
        #[arg(value_name = "auto|yes|no")]
        value: String,
    },

    /// Mark posts as approved
    Approve {
        #[arg(required = true)]
        ids: Vec<i64>,
        /// Total points for one guest suggestion (5–20)
        #[arg(long)]
        points: Option<i64>,
    },

    /// Mark posts as rejected
    Reject {
        #[arg(required = true)]
        ids: Vec<i64>,
    },

    /// Delete a post and its media references
    Rm { id: i64 },

    /// Manage channels
    Channel {
        #[command(subcommand)]
        cmd: ChannelCmd,
    },

    /// Manage the publish queue
    Queue {
        #[command(subcommand)]
        cmd: QueueCmd,
    },

    /// Render what the next slots would publish
    Preview {
        #[arg(value_name = "CHANNEL")]
        channel: String,
        #[arg(long, default_value_t = 10)]
        count: usize,
    },

    /// Publish the next queued post for a channel
    Push {
        #[arg(long, value_name = "CHANNEL")]
        channel: String,
        /// Render the payload without touching Telegram
        #[arg(long)]
        dry_run: bool,
        /// Ignore the schedule and the daily cap
        #[arg(long)]
        force: bool,
    },

    /// Publish all queued posts for a channel in order, ignoring the schedule
    Flush {
        #[arg(long, value_name = "CHANNEL")]
        channel: String,
    },

    /// Run the scheduler (and the admin UI) until interrupted
    Run {
        /// One scheduler pass, then exit
        #[arg(long)]
        once: bool,
        /// Seconds between scheduler ticks
        #[arg(long, default_value_t = 15)]
        tick: u64,
        /// Do not start the admin UI
        #[arg(long)]
        no_web: bool,
        /// Do not start the Telegram command interface
        #[arg(long)]
        no_bot: bool,
        #[arg(long, value_name = "ADDR")]
        bind: Option<String>,
    },

    /// Serve only the admin UI
    Serve {
        #[arg(long, value_name = "ADDR")]
        bind: Option<String>,
    },

    /// Run the Telegram command interface on its own
    Bot {
        /// Handle the queued updates once, then exit
        #[arg(long)]
        once: bool,
    },

    /// External storage mirrors (local folder, WebDAV, Telegram)
    Storage {
        #[command(subcommand)]
        cmd: StorageCmd,
    },

    /// Bot users and their roles
    Users {
        #[command(subcommand)]
        cmd: UsersCmd,
    },

    /// Reclaim unreferenced files or clean up confirmed sent posts
    Gc {
        #[arg(long)]
        dry_run: bool,
        /// Preview cleanup of confirmed sent posts
        #[arg(long)]
        sent: bool,
        /// Age of the latest confirmed delivery (default 30 days)
        #[arg(long, requires = "sent")]
        older_than_days: Option<u32>,
        /// Apply sent-post cleanup
        #[arg(long, requires = "sent", conflicts_with = "dry_run")]
        yes: bool,
    },

    /// Write a backup (database snapshot, JSONL metadata, optionally media)
    Export {
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Also copy the blob store
        #[arg(long)]
        with_media: bool,
        /// Export only outgoing artwork and its storage metadata
        #[arg(long, conflicts_with = "with_media", requires = "out")]
        share: bool,
    },

    /// Tag aliases and content rules
    Alias {
        #[command(subcommand)]
        cmd: AliasCmd,
    },

    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },

    /// Inspect or override the local character-to-franchise catalog.
    Tagger {
        #[command(subcommand)]
        cmd: TaggerCmd,
    },

    /// Show the send log
    Log {
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaggerCmd {
    /// Add or replace an operator's franchise mapping for a character ID.
    Map {
        #[arg(value_parser = clap::value_parser!(i64).range(1..))]
        character_id: i64,
        franchise: String,
        /// Bind the exact character label returned by FoidFinder for offline use.
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove an operator override and use the catalog or cache again.
    Unmap {
        #[arg(value_parser = clap::value_parser!(i64).range(1..))]
        character_id: i64,
    },
    /// List effective mappings and where they came from.
    Mappings,
}

#[derive(Debug, Subcommand)]
pub enum ChannelCmd {
    /// Add a channel: `channel add [alias] <@name|id>`
    Add {
        /// Short handle used by the bot and the CLI (letters, digits, - and _)
        #[arg(
            value_name = "ALIAS",
            id = "alias_positional",
            allow_negative_numbers = true
        )]
        alias: Option<String>,
        /// Channel `@username` or numeric chat id
        #[arg(value_name = "CHAT", allow_negative_numbers = true)]
        chat: Option<String>,
        /// Skip the getChat validation call
        #[arg(long)]
        no_verify: bool,
        #[command(flatten)]
        opts: ChannelOpts,
    },
    /// List channels with queue depth and schedule state
    Ls,
    /// Update a channel
    Edit {
        #[arg(value_name = "CHANNEL")]
        channel: String,
        #[command(flatten)]
        opts: ChannelOpts,
    },
    /// Remove a channel (and its queue)
    Rm {
        #[arg(value_name = "CHANNEL")]
        channel: String,
    },
    /// Stop publishing to a channel
    Pause {
        #[arg(value_name = "CHANNEL")]
        channel: String,
    },
    /// Resume publishing
    Resume {
        #[arg(value_name = "CHANNEL")]
        channel: String,
    },
    /// Channels the bot was added to and nobody named yet
    Discover,
    /// Start publishing to a discovered channel
    Enable {
        #[arg(value_name = "CHANNEL")]
        channel: String,
    },
    /// Stop publishing to a channel (keeps its queue)
    Disable {
        #[arg(value_name = "CHANNEL")]
        channel: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum QueueCmd {
    /// Queue posts into a channel
    Add {
        #[arg(required = true)]
        ids: Vec<i64>,
        #[arg(long, value_name = "CHANNEL")]
        channel: String,
        #[arg(long, default_value_t = 0)]
        priority: i64,
    },
    /// Show the queue
    Ls {
        #[arg(long, value_name = "CHANNEL")]
        channel: Option<String>,
        #[arg(long, value_name = "STATE")]
        state: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Skip an item (kept for the record)
    Skip { id: i64 },
    /// Put an item first / back to normal
    Pin {
        id: i64,
        #[arg(long)]
        off: bool,
    },
    /// Put a finished item back in the queue
    Reset { id: i64 },
    /// Remove an item from the queue
    Rm { id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum StorageCmd {
    /// Configured sinks and how many posts each one holds
    Ls,
    /// Check that every sink is reachable and writable
    Test,
    /// Queue and upload posts that are not mirrored yet
    Sync {
        /// Only for this sink name (local, webdav, telegram)
        #[arg(long)]
        sink: Option<String>,
        /// Only this post
        #[arg(long)]
        post: Option<i64>,
        /// How many posts to consider
        #[arg(long, default_value_t = 500)]
        limit: i64,
        /// Report what would happen without uploading
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum UsersCmd {
    /// List users the bot has seen
    Ls,
    /// Set a role: admin, guest or banned
    Grant {
        tg_id: i64,
        #[arg(value_name = "admin|guest|banned")]
        role: String,
    },
    /// Deny a user
    Ban { tg_id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum AliasCmd {
    /// Map a tag to another tag (applies to future imports)
    Set { from: String, to: String },
}

#[derive(Debug, Subcommand)]
pub enum RuleCmd {
    /// Add a rule: block (never publish) or nsfw (force NSFW)
    Add {
        tag: String,
        /// block | nsfw
        action: String,
    },
    Ls,
    Rm {
        tag: String,
    },
}

#[derive(Debug, Clone, Default, ClapArgs)]
pub struct ChannelOpts {
    /// Alias (handle) as well, for `channel edit`
    #[arg(long)]
    pub alias: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, value_name = "@name|-100123…", allow_negative_numbers = true)]
    pub tg_id: Option<String>,
    /// Minutes between posts
    #[arg(long)]
    pub interval: Option<i64>,
    #[arg(long, value_name = "HH:MM")]
    pub active_from: Option<String>,
    #[arg(long, value_name = "HH:MM")]
    pub active_to: Option<String>,
    #[arg(long)]
    pub tz: Option<String>,
    /// 0 = unlimited
    #[arg(long)]
    pub daily_cap: Option<i64>,
    #[arg(long)]
    pub jitter: Option<i64>,
    #[arg(long)]
    pub artist_cooldown: Option<usize>,
    /// allow | spoil | reject
    #[arg(long)]
    pub nsfw: Option<String>,
    #[arg(long)]
    pub min_media: Option<i64>,
    #[arg(long, value_name = "tag1,tag2")]
    pub blocked_tags: Option<String>,
    #[arg(long, value_name = "tag1,tag2")]
    pub required_tags: Option<String>,
    /// hashtags | plain | none
    #[arg(long)]
    pub tag_style: Option<String>,
    #[arg(long)]
    pub max_tags: Option<i64>,
    /// footer | link | none
    #[arg(long)]
    pub attribution: Option<String>,
    /// html | markdownv2 | plain
    #[arg(long)]
    pub parse_mode: Option<String>,
    #[arg(long)]
    pub template_name: Option<String>,
    /// Read the caption template from a file
    #[arg(long, value_name = "FILE")]
    pub template_file: Option<PathBuf>,
    #[arg(long)]
    pub silent: Option<bool>,
    #[arg(long)]
    pub protect: Option<bool>,
    #[arg(long)]
    pub include_source_tag: Option<bool>,
    #[arg(long)]
    pub posts_per_slot: Option<i64>,
    #[arg(long)]
    pub enabled: Option<bool>,
}

impl ChannelOpts {
    fn apply(&self, channel: &mut Channel) -> Result<()> {
        if let Some(alias) = &self.alias {
            channel.alias = crate::model::slugify(alias);
        }
        if let Some(title) = &self.title {
            channel.title = title.clone();
        }
        if let Some(tg_id) = &self.tg_id {
            channel.tg_id = tg_id.clone();
        }
        if let Some(interval) = self.interval {
            if interval < 1 {
                return Err(Error::invalid("--interval must be at least 1 minute"));
            }
            channel.interval_minutes = interval;
        }
        if let Some(from) = &self.active_from {
            crate::queue::scheduler::parse_hhmm(from)?;
            channel.active_from = from.clone();
        }
        if let Some(to) = &self.active_to {
            crate::queue::scheduler::parse_hhmm(to)?;
            channel.active_to = to.clone();
        }
        if let Some(tz) = &self.tz {
            tz.parse::<chrono_tz::Tz>()
                .map_err(|_| Error::invalid(format!("unknown timezone: {tz}")))?;
            channel.tz = tz.clone();
        }
        if let Some(cap) = self.daily_cap {
            channel.daily_cap = cap.max(0);
        }
        if let Some(jitter) = self.jitter {
            channel.jitter_secs = jitter.max(0);
        }
        if let Some(cooldown) = self.artist_cooldown {
            channel.artist_cooldown = cooldown as i64;
        }
        if let Some(policy) = &self.nsfw {
            channel.nsfw_policy = policy.parse()?;
        }
        if let Some(min_media) = self.min_media {
            channel.min_media = min_media.max(0);
        }
        if let Some(tags) = &self.blocked_tags {
            channel.blocked_tags = crate::model::join_csv(crate::model::split_csv(tags));
        }
        if let Some(tags) = &self.required_tags {
            channel.required_tags = crate::model::join_csv(crate::model::split_csv(tags));
        }
        if let Some(style) = &self.tag_style {
            channel.tag_style = style.parse::<TagStyle>()?;
        }
        if let Some(max_tags) = self.max_tags {
            channel.max_tags = max_tags.max(0);
        }
        if let Some(attribution) = &self.attribution {
            channel.attribution = attribution.parse::<Attribution>()?;
        }
        if let Some(mode) = &self.parse_mode {
            channel.parse_mode = mode.parse::<ParseMode>()?;
        }
        if let Some(name) = &self.template_name {
            channel.template_name = name.clone();
        }
        if let Some(path) = &self.template_file {
            let source = std::fs::read_to_string(path)
                .map_err(|e| Error::config(format!("cannot read {}: {e}", path.display())))?;
            channel.template = Some(source);
        }
        if let Some(silent) = self.silent {
            channel.silent = silent;
        }
        if let Some(protect) = self.protect {
            channel.protect = protect;
        }
        if let Some(include) = self.include_source_tag {
            channel.include_source_tag = include;
        }
        if let Some(per_slot) = self.posts_per_slot {
            channel.posts_per_slot = per_slot.max(1);
        }
        if let Some(enabled) = self.enabled {
            channel.enabled = enabled;
        }
        Ok(())
    }
}

pub async fn run() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

    let cfg = match Config::load(args.home.as_deref()) {
        Ok(cfg) => cfg,
        Err(err) => {
            return Err(Error::config(format!(
                "{err}\nrun `nujijushi init` to create a data directory"
            )));
        }
    };

    match args.cmd {
        Command::Init { bot_token, force } => cmd_init(cfg, bot_token, force),
        Command::Doctor { strict, offline } => cmd_doctor(cfg, strict, offline).await,
        Command::Serve { bind } => {
            let state = open_state(cfg)?;
            crate::jobs::worker::reconcile_startup(&state).await?;
            println!("scheduler  not running in `serve` - `nujijushi run` publishes on schedule");
            monitor_notifications(&state, crate::web::serve(state.clone(), bind)).await
        }
        Command::Bot { once } => {
            let state = open_state(cfg)?;
            if state.transport.is_none() {
                return Err(Error::config(
                    "the bot interface needs a token: set TG_BOT_TOKEN",
                ));
            }
            if once {
                cmd_bot(&state, true).await?;
                queue_alerts::check(&state).await;
                reputation_notifications::check(&state).await;
                Ok(())
            } else {
                monitor_notifications(&state, cmd_bot(&state, false)).await
            }
        }
        Command::Gc {
            dry_run,
            sent,
            older_than_days,
            yes,
        } => cmd_gc(cfg, dry_run, sent, older_than_days, yes).await,
        Command::Export {
            out,
            with_media,
            share,
        } => {
            if share {
                let dir = out.ok_or_else(|| Error::invalid("--share requires --out"))?;
                let state = open_state(cfg)?;
                let (written, skipped) = crate::storage::share(&state, &dir).await?;
                println!(
                    "Exported {written} post(s) to {} · {skipped} without outgoing media",
                    dir.display()
                );
                Ok(())
            } else {
                cmd_export(cfg, out, with_media).await
            }
        }
        Command::Alias { cmd } => {
            let state = open_state(cfg)?;
            cmd_alias(&state, cmd).await
        }
        Command::Rule { cmd } => {
            let state = open_state(cfg)?;
            cmd_rule(&state, cmd).await
        }
        Command::Users { cmd } => {
            let state = open_state(cfg)?;
            cmd_users(&state, cmd).await
        }
        Command::Storage { cmd } => {
            let state = open_state(cfg)?;
            cmd_storage(&state, cmd).await
        }
        other => {
            let state = open_state(cfg)?;
            dispatch(&state, other).await
        }
    }
}

async fn monitor_notifications<T>(
    state: &AppState,
    command: impl std::future::Future<Output = T>,
) -> T {
    reputation_notifications::monitor_while(state, queue_alerts::monitor_while(state, command))
        .await
}

async fn dispatch(state: &AppState, cmd: Command) -> Result<()> {
    match cmd {
        Command::Import {
            targets,
            channel,
            priority,
        } => cmd_import(state, targets, channel, priority).await,
        Command::Ls {
            state: filter_state,
            source,
            tag,
            nsfw,
            search,
            limit,
        } => cmd_ls(state, filter_state, source, tag, nsfw, search, limit).await,
        Command::Show { id, channel, raw } => cmd_show(state, id, channel, raw).await,
        Command::Tag { id, tags, remove } => cmd_tag(state, id, tags, remove),
        Command::Tagger { cmd } => cmd_tagger(state, cmd),
        Command::Rate { id, rating } => cmd_rate(state, id, rating),
        Command::Nsfw { id, value } => cmd_nsfw(state, id, value),
        Command::Approve { ids, points } => {
            cmd_set_state(state, ids, PostState::Approved, points).await
        }
        Command::Reject { ids } => cmd_set_state(state, ids, PostState::Rejected, None).await,
        Command::Rm { id } => {
            let removed = state
                .db
                .call(move |conn| crate::db::posts::delete(conn, id))
                .await?;
            if removed {
                println!("deleted post #{id}");
            } else {
                return Err(Error::NotFound(format!("post {id}")));
            }
            Ok(())
        }
        Command::Channel { cmd } => cmd_channel(state, cmd).await,
        Command::Queue { cmd } => cmd_queue(state, cmd).await,
        Command::Preview { channel, count } => cmd_preview(state, channel, count).await,
        Command::Push {
            channel,
            dry_run,
            force,
        } => cmd_push(state, channel, dry_run, force).await,
        Command::Flush { channel } => {
            let channel = resolve_channel(state, &channel).await?;
            let result = crate::tg::publish::flush_channel(state, channel.id, None).await;
            queue_alerts::check(state).await;
            println!("{}", result?.describe());
            Ok(())
        }
        Command::Run {
            once,
            tick,
            no_web,
            no_bot,
            bind,
        } => {
            let command = cmd_run(state, once, tick, no_web, no_bot, bind);
            if once {
                command.await?;
                queue_alerts::check(state).await;
                reputation_notifications::check(state).await;
                Ok(())
            } else {
                monitor_notifications(state, command).await
            }
        }
        Command::Log { limit } => cmd_log(state, limit).await,
        Command::Init { .. }
        | Command::Doctor { .. }
        | Command::Serve { .. }
        | Command::Gc { .. }
        | Command::Export { .. }
        | Command::Alias { .. }
        | Command::Rule { .. }
        | Command::Bot { .. }
        | Command::Users { .. }
        | Command::Storage { .. } => {
            unreachable!("handled before dispatch")
        }
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("nujijushi={default},warn"))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

fn open_state(cfg: Config) -> Result<AppState> {
    cfg.ensure_layout()?;
    let transport = tg::build_transport(&cfg)?;
    AppState::open(cfg, transport)
}

// ---------------------------------------------------------------- init

/// What `init` may print as the UI token: the value it just wrote, the token
/// already in the config, or a warning that none exists. Printing a freshly
/// generated token when the config already existed was misleading.
fn displayed_ui_token<'a>(
    wrote: bool,
    configured: Option<&'a str>,
    generated: Option<&'a str>,
) -> &'a str {
    match (wrote, configured, generated) {
        (true, _, Some(token)) => token,
        (false, Some(token), _) => token,
        _ => "(not set - add [web] token to config.toml or set NUJIJUSHI_UI_TOKEN)",
    }
}

fn cmd_init(cfg: Config, bot_token: Option<String>, force: bool) -> Result<()> {
    cfg.ensure_layout()?;

    let token = bot_token
        .or_else(|| cfg.bot_token.clone())
        .filter(|t| !t.trim().is_empty());

    let mut effective = cfg.clone();
    if effective.ui_token.is_none() {
        effective.ui_token = Some(crate::config::random_token());
    }

    let config_path = cfg.home.join("config.toml");
    if force && config_path.exists() {
        std::fs::remove_file(&config_path)?;
    }
    let wrote = effective.write_default_config(token.as_deref())?;

    let db = crate::db::Db::open(cfg.db_path())?;
    let version = db.schema_version()?;

    println!("nujijushi {} initialized", crate::VERSION);
    println!("  home      {}", cfg.home.display());
    println!(
        "  config    {}{}",
        config_path.display(),
        if wrote { " (created)" } else { " (existing)" }
    );
    println!(
        "  database  {} (schema v{version})",
        cfg.db_path().display()
    );
    println!("  blobs     {}", cfg.blobs_dir().display());
    println!(
        "  bot token {}",
        if token.is_some() { "set" } else { "missing" }
    );
    println!(
        "  ui token  {}",
        displayed_ui_token(
            wrote,
            cfg.ui_token.as_deref(),
            effective.ui_token.as_deref()
        )
    );
    println!();
    println!("Next steps:");
    if token.is_none() {
        println!("  1. put your bot token in config.toml or export TG_BOT_TOKEN");
        println!("  2. nujijushi channel add --title \"My art\" --tg-id @mychannel");
    } else {
        println!("  1. nujijushi channel add --title \"My art\" --tg-id @mychannel");
    }
    println!(
        "  *  add cookies to {}/pixiv.txt, twitter.txt",
        cfg.cookies_dir().display()
    );
    println!("  *  nujijushi doctor");
    Ok(())
}

// ---------------------------------------------------------------- doctor

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Check {
    level: Level,
    label: String,
    detail: String,
}

impl Check {
    fn print(&self) {
        let marker = match self.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        if self.detail.is_empty() {
            println!("  [{marker}] {}", self.label);
        } else {
            println!("  [{marker}] {}: {}", self.label, self.detail);
        }
    }
}

async fn cmd_doctor(cfg: Config, strict: bool, offline: bool) -> Result<()> {
    let mut checks: Vec<Check> = Vec::new();

    let home_ok = cfg.home.exists() && cfg.home.is_dir();
    checks.push(Check {
        level: if home_ok { Level::Ok } else { Level::Fail },
        label: "data directory".into(),
        detail: cfg.home.display().to_string(),
    });

    let mut channels: Vec<Channel> = Vec::new();
    match crate::db::Db::open(cfg.db_path()) {
        Ok(db) => {
            let version = db.schema_version()?;
            let posts = db.with(|conn| crate::db::posts::count(conn, None))?;
            channels = db.with(|conn| crate::db::channels::list(conn))?;
            checks.push(Check {
                level: Level::Ok,
                label: "database".into(),
                detail: format!(
                    "schema v{version}, {} post(s), {} channel(s)",
                    posts,
                    channels.len()
                ),
            });
            for channel in &channels {
                let queued = db.with(|conn| {
                    crate::db::queue::count_state(conn, channel.id, QueueState::Queued)
                })?;
                let mut note = format!("{queued} queued");
                if channel.paused {
                    note.push_str(", PAUSED");
                }
                if channel.consecutive_failures > 0 {
                    note.push_str(&format!(
                        ", {} consecutive failures",
                        channel.consecutive_failures
                    ));
                }
                checks.push(Check {
                    level: if channel.paused {
                        Level::Warn
                    } else {
                        Level::Ok
                    },
                    label: format!("channel {}", channel.title),
                    detail: format!("{} ({note})", channel.tg_id),
                });
            }
        }
        Err(err) => checks.push(Check {
            level: Level::Fail,
            label: "database".into(),
            detail: err.to_string(),
        }),
    }
    if let Ok(db) = crate::db::Db::open(cfg.db_path()) {
        let discovered = db
            .with(|conn| crate::db::channels::list_discovered(conn))
            .map(|channels| channels.iter().filter(|c| !c.enabled).count())
            .unwrap_or(0);
        checks.push(Check {
            level: if discovered > 0 {
                Level::Warn
            } else {
                Level::Ok
            },
            label: "discovered channels".into(),
            detail: if discovered > 0 {
                format!("{discovered} waiting for `nujijushi channel enable <alias>`")
            } else {
                "none pending".into()
            },
        });
        let users = db.with(|conn| crate::db::users::count(conn)).unwrap_or(0);
        checks.push(Check {
            level: Level::Ok,
            label: "bot users".into(),
            detail: format!("{users} known"),
        });
    }

    checks.push(tool_check("gallery-dl", &cfg.gallery_dl, &["--version"]).await);
    checks.push(tool_check("ffmpeg", &cfg.ffmpeg, &["-version"]).await);
    let (level, detail) = match cfg.pixiv_backend {
        crate::config::PixivBackend::GalleryDl => (Level::Ok, "gallery-dl".into()),
        crate::config::PixivBackend::Native => match crate::source::pixiv::session_cookie(&cfg) {
            Ok(Some(_)) => (Level::Ok, "native with cookies/pixiv.txt".into()),
            Ok(None) => (Level::Ok, "native with guest access".into()),
            Err(error) => (Level::Warn, error.to_string()),
        },
    };
    checks.push(Check {
        label: "Pixiv backend".into(),
        level,
        detail,
    });
    let (level, detail) = match cfg.twitter_backend {
        crate::config::TwitterBackend::GalleryDl => (Level::Ok, "gallery-dl".into()),
        crate::config::TwitterBackend::Native => match crate::source::twitter::session(&cfg) {
            Ok(Some(_)) => (Level::Ok, "native with an X cookie file".into()),
            Ok(None) => (
                Level::Ok,
                "native selected, guest imports use gallery-dl".into(),
            ),
            Err(error) => (Level::Warn, error.to_string()),
        },
    };
    checks.push(Check {
        label: "X backend".into(),
        level,
        detail,
    });
    let (level, detail) =
        match crate::source::rednote::session(&cfg, chrono::Utc::now().timestamp()) {
            Ok(Some(_)) => (Level::Ok, "native with cookies/rednote.txt".into()),
            Ok(None) => (Level::Ok, "native with guest access".into()),
            Err(error) => (Level::Warn, error.to_string()),
        };
    checks.push(Check {
        label: "RedNote backend".into(),
        level,
        detail,
    });
    let tagger = &cfg.tag_suggestions;
    let (level, detail) = if tagger.enabled {
        match crate::jobs::tag_suggestions::health(tagger.tagger_port).await {
            Ok(()) => (
                Level::Ok,
                format!("ready on 127.0.0.1:{}", tagger.tagger_port),
            ),
            Err(err) => (Level::Warn, err.to_string()),
        }
    } else {
        (Level::Ok, "disabled".into())
    };
    checks.push(Check {
        level,
        label: "FoidFinder".into(),
        detail,
    });

    let cookies = cfg.cookies_dir();
    let mut found_cookies = 0;
    if let Ok(entries) = std::fs::read_dir(&cookies) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            found_cookies += 1;
            let age_days = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|mtime| std::time::SystemTime::now().duration_since(mtime).ok())
                .map(|age| age.as_secs() / 86_400);
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            checks.push(match age_days {
                Some(days) if days > 30 => Check {
                    level: Level::Warn,
                    label: format!("cookie {name}"),
                    detail: format!("{days} days old - sites may reject it, re-export"),
                },
                Some(days) => Check {
                    level: Level::Ok,
                    label: format!("cookie {name}"),
                    detail: format!("{days} days old"),
                },
                None => Check {
                    level: Level::Ok,
                    label: format!("cookie {name}"),
                    detail: String::new(),
                },
            });
            if let Some(warning) = crate::config::loose_permissions(&path) {
                checks.push(Check {
                    level: Level::Warn,
                    label: format!("cookie {name} perms"),
                    detail: warning,
                });
            }
        }
    }
    if found_cookies == 0 {
        checks.push(Check {
            level: Level::Warn,
            label: "cookies".into(),
            detail: format!(
                "none in {} (public content imports fine without them)",
                cookies.display()
            ),
        });
    }
    if let Some(warning) = crate::config::loose_permissions(&cfg.home.join("config.toml")) {
        checks.push(Check {
            level: Level::Warn,
            label: "config perms".into(),
            detail: warning,
        });
    }

    checks.push(match cfg.bot_token.as_deref() {
        Some(token) if !token.trim().is_empty() => {
            let shape_ok = token
                .split_once(':')
                .map(|(a, b)| a.chars().all(|c| c.is_ascii_digit()) && b.len() >= 30);
            Check {
                level: if shape_ok == Some(true) {
                    Level::Ok
                } else {
                    Level::Warn
                },
                label: "bot token".into(),
                detail: if shape_ok == Some(true) {
                    "present".into()
                } else {
                    "present but not shaped like <digits>:<secret>".into()
                },
            }
        }
        _ => Check {
            level: Level::Warn,
            label: "bot token".into(),
            detail: "missing - publishing is disabled (TG_BOT_TOKEN)".into(),
        },
    });
    checks.push(Check {
        level: if cfg.ui_token.is_some() {
            Level::Ok
        } else {
            Level::Warn
        },
        label: "ui token".into(),
        detail: if cfg.ui_token.is_some() {
            "set".into()
        } else {
            "missing - the admin UI will refuse to start".into()
        },
    });
    let loopback_ui = cfg.bind.starts_with("127.")
        || cfg.bind.starts_with("localhost")
        || cfg.bind.starts_with("[::1]");
    let (cookie_level, cookie_detail) = if cfg.secure_cookies {
        (Level::Ok, "enabled".to_string())
    } else if loopback_ui {
        (Level::Ok, "off (loopback UI)".to_string())
    } else {
        (
            Level::Warn,
            format!(
                "off while binding {}; set [web] secure_cookies when a TLS proxy serves it",
                cfg.bind
            ),
        )
    };
    checks.push(Check {
        level: cookie_level,
        label: "secure cookies".into(),
        detail: cookie_detail,
    });
    checks.push(Check {
        level: Level::Ok,
        label: "web base path".into(),
        detail: format!("{}/", cfg.web_base_path),
    });

    checks.push(match crate::http::client(&cfg) {
        Ok(_) if cfg.proxy_enabled() => Check {
            level: Level::Ok,
            label: "proxy".into(),
            detail: format!(
                "{} (telegram, storage uploads and t.me fetches)",
                cfg.proxy_url.as_deref().unwrap_or_default()
            ),
        },
        Ok(_) => Check {
            level: Level::Ok,
            label: "proxy".into(),
            detail: "none configured (direct; the http client still honours HTTPS_PROXY)".into(),
        },
        Err(err) => Check {
            level: Level::Fail,
            label: "proxy".into(),
            detail: err.to_string(),
        },
    });
    if cfg
        .proxy_url
        .as_deref()
        .map(|url| url.starts_with("socks"))
        .unwrap_or(false)
    {
        checks.push(Check {
            level: Level::Warn,
            label: "proxy note".into(),
            detail: "gallery-dl needs PySocks for socks proxies: pipx inject gallery-dl PySocks"
                .into(),
        });
    }

    let mut storage_notes: Vec<String> = Vec::new();
    if let Some(local) = &cfg.storage.local {
        storage_notes.push(format!("local:{}", local.path.display()));
    }
    if let Some(webdav) = &cfg.storage.webdav {
        storage_notes.push(format!("webdav:{}", webdav.url));
    }
    if let Some(telegram) = &cfg.storage.telegram {
        storage_notes.push(format!("telegram:{}", telegram.chat_id));
    }
    checks.push(Check {
        level: if storage_notes.is_empty() {
            Level::Warn
        } else {
            Level::Ok
        },
        label: "storage sinks".into(),
        detail: if storage_notes.is_empty() {
            "none enabled (mirrors stay local); set [storage.local] and friends".into()
        } else {
            format!(
                "{} (on_import = {})",
                storage_notes.join(", "),
                cfg.storage.on_import
            )
        },
    });
    if cfg.storage.telegram.is_some() && cfg.bot_token.is_none() {
        checks.push(Check {
            level: Level::Fail,
            label: "telegram storage".into(),
            detail: "enabled but TG_BOT_TOKEN is missing".into(),
        });
    }
    let config_file = cfg.home.join("config.toml");
    checks.push(Check {
        level: if cfg.access.admins.is_empty() && !cfg.access.public {
            Level::Warn
        } else {
            Level::Ok
        },
        label: "bot access".into(),
        detail: if cfg.access.admins.is_empty() && !cfg.access.public {
            format!(
                "no admins and public = false: nobody can use the bot (set NUJIJUSHI_ADMINS or [access] admins in {})",
                config_file.display()
            )
        } else {
            // admin and "may publish from chat" are two separate switches; say so
            // here, because a refused /publish is otherwise invisible until you try
            let publish_hint = if cfg.access.allow_publish {
                String::new()
            } else {
                format!(
                    " - publishing from Telegram is off ([access] allow_publish in {})",
                    config_file.display()
                )
            };
            format!(
                "{} admin(s), public={}, import={} browse={} queue={} publish={} curate={}{publish_hint}",
                cfg.access.admins.len(),
                cfg.access.public,
                cfg.access.allow_import,
                cfg.access.allow_browse,
                cfg.access.allow_queue,
                cfg.access.allow_publish,
                cfg.access.allow_curate
            )
        },
    });
    checks.push(Check {
        level: if cfg.bot_enabled {
            Level::Ok
        } else {
            Level::Warn
        },
        label: "bot interface".into(),
        detail: if cfg.bot_enabled {
            format!(
                "enabled, {} mode (started by `nujijushi run`)",
                cfg.bot_mode.as_str()
            )
        } else {
            "disabled in config ([bot] enabled = false)".into()
        },
    });
    checks.push(Check {
        level: if cfg.guest_reputation && cfg.bot_token.is_none() {
            Level::Warn
        } else {
            Level::Ok
        },
        label: "guest reputation".into(),
        detail: match (cfg.guest_reputation, cfg.bot_token.is_some()) {
            (false, _) => "off ([bot] guest_reputation = false)",
            (true, true) => "enabled",
            (true, false) => "enabled, but Telegram notifications require TG_BOT_TOKEN",
        }
        .into(),
    });
    let alert_admins = queue_alerts::recipients(&cfg.access.admins).len();
    let alert_token = cfg
        .bot_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    let (alert_level, alert_detail) = match (cfg.queue_alerts, alert_token, alert_admins) {
        (false, _, _) => (Level::Ok, "off ([bot] queue_alerts = false)".into()),
        (true, false, 0) => (
            Level::Warn,
            "enabled, but a bot token and [access] admins are required".into(),
        ),
        (true, false, _) => (Level::Warn, "enabled, but a bot token is required".into()),
        (true, true, 0) => (
            Level::Warn,
            "enabled, but [access] admins needs a Telegram user ID".into(),
        ),
        (true, true, count) => (
            Level::Ok,
            format!(
                "enabled for {count} configured admin(s); each admin must start a private chat with the bot"
            ),
        ),
    };
    checks.push(Check {
        level: alert_level,
        label: "queue alerts".into(),
        detail: alert_detail,
    });
    if cfg.bot_mode == crate::config::BotMode::Webhook {
        checks.push(match crate::bot::webhook_target(&cfg) {
            Ok(url) => Check {
                level: Level::Ok,
                label: "webhook target".into(),
                detail: url,
            },
            Err(err) => Check {
                level: Level::Fail,
                label: "webhook target".into(),
                detail: err.to_string(),
            },
        });
        checks.push(Check {
            level: Level::Warn,
            label: "webhook reachability".into(),
            detail: format!(
                "Telegram must reach that URL over HTTPS: put a TLS proxy in front of [web] bind {}",
                cfg.bind
            ),
        });
    }

    if offline {
        checks.push(Check {
            level: Level::Warn,
            label: "telegram api".into(),
            detail: "skipped (--offline)".into(),
        });
    } else {
        match tg::build_transport(&cfg) {
            Ok(Some(transport)) => {
                checks.push(match transport.me().await {
                    Ok(me) => Check {
                        level: Level::Ok,
                        label: "telegram api".into(),
                        detail: format!("authenticated as {me}"),
                    },
                    Err(err) => Check {
                        level: Level::Fail,
                        label: "telegram api".into(),
                        detail: err.to_string(),
                    },
                });
                checks.push(check_channel_rights(&channels, transport.as_ref()).await);
                let mode = cfg.bot_mode;
                let target = crate::bot::webhook_target(&cfg);
                let webhook = transport.webhook_info().await;
                checks.push(match (mode, &webhook) {
                    (crate::config::BotMode::Webhook, Ok(info)) => match &target {
                        Ok(expected) if info.url == *expected => Check {
                            level: Level::Ok,
                            label: "webhook".into(),
                            detail: format!("Telegram posts updates to {expected}"),
                        },
                        Ok(expected) if info.url.is_empty() => Check {
                            level: Level::Warn,
                            label: "webhook".into(),
                            detail: format!(
                                "not registered yet ({expected} expected) - `nujijushi run` registers it"
                            ),
                        },
                        Ok(expected) => Check {
                            level: Level::Warn,
                            label: "webhook".into(),
                            detail: format!("Telegram posts to {} instead of {expected}", info.url),
                        },
                        Err(err) => Check {
                            level: Level::Fail,
                            label: "webhook".into(),
                            detail: err.to_string(),
                        },
                    },
                    (crate::config::BotMode::Webhook, Err(err)) => Check {
                        level: Level::Warn,
                        label: "webhook".into(),
                        detail: err.to_string(),
                    },
                    (crate::config::BotMode::Polling, Ok(info)) if info.is_set() => Check {
                        level: Level::Warn,
                        label: "long polling".into(),
                        detail: format!(
                            "a webhook points at {} - `nujijushi run` deletes it before polling",
                            info.url
                        ),
                    },
                    (crate::config::BotMode::Polling, Ok(_)) => Check {
                        level: Level::Ok,
                        label: "long polling".into(),
                        detail: "no webhook registered, the bot can poll".into(),
                    },
                    (crate::config::BotMode::Polling, Err(err)) => Check {
                        level: Level::Warn,
                        label: "long polling".into(),
                        detail: err.to_string(),
                    },
                });
                // A registered URL proves nothing: Telegram only learns that it
                // cannot deliver when it POSTs (502, 404, bad secret, TLS).
                if mode == crate::config::BotMode::Webhook
                    && let Ok(info) = &webhook
                {
                    let now = chrono::Utc::now().timestamp();
                    checks.push(match info.delivery_problem(now) {
                        Some(problem) => Check {
                            level: Level::Fail,
                            label: "webhook delivery".into(),
                            detail: format!(
                                "{problem} - {} update(s) queued; check nginx, the port in webhook_url and the secret",
                                info.pending_update_count
                            ),
                        },
                        None if info.pending_update_count > 0 => Check {
                            level: Level::Warn,
                            label: "webhook delivery".into(),
                            detail: format!("{} update(s) still queued", info.pending_update_count),
                        },
                        None => Check {
                            level: Level::Ok,
                            label: "webhook delivery".into(),
                            detail: "Telegram reports no delivery errors".into(),
                        },
                    });
                }
            }
            Ok(None) => checks.push(Check {
                level: Level::Warn,
                label: "telegram api".into(),
                detail: "skipped (no bot token)".into(),
            }),
            Err(err) => checks.push(Check {
                level: Level::Fail,
                label: "telegram api".into(),
                detail: err.to_string(),
            }),
        }
    }

    let store = crate::blob::BlobStore::new(cfg.blobs_dir(), cfg.thumbs_dir())?;
    checks.push(Check {
        level: Level::Ok,
        label: "media storage".into(),
        detail: format!("{:.1} MiB on disk", store.disk_usage() as f64 / 1_048_576.0),
    });

    if let Ok(db) = crate::db::Db::open(cfg.db_path()) {
        for stale in db.with(|conn| crate::db::jobs::list(conn, 5))? {
            if stale.is_active() {
                checks.push(Check {
                    level: Level::Warn,
                    label: "import job".into(),
                    detail: format!("#{} {} still {}", stale.id, stale.kind, stale.state),
                });
            }
        }
        let unknown = db.with(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM queue WHERE state = 'unknown'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })?;
        if unknown > 0 {
            checks.push(Check {
                level: Level::Warn,
                label: "unconfirmed sends".into(),
                detail: format!("{unknown} queue item(s) need a manual decision (see the UI)"),
            });
        }
    }

    println!("nujijushi {} doctor", crate::VERSION);
    println!();
    let failures = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warnings = checks.iter().filter(|c| c.level == Level::Warn).count();
    for check in &checks {
        check.print();
    }
    println!();
    if failures == 0 && warnings == 0 {
        println!("everything looks good.");
    } else {
        println!("{failures} failure(s), {warnings} warning(s)");
    }

    if strict && failures > 0 {
        return Err(Error::other(format!("{failures} doctor check(s) failed")));
    }
    Ok(())
}

async fn tool_check(name: &str, program: &str, args: &[&str]) -> Check {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let first_line = text.lines().next().unwrap_or("").trim().to_string();
            Check {
                level: Level::Ok,
                label: name.into(),
                detail: if first_line.is_empty() {
                    program.to_string()
                } else {
                    first_line.chars().take(60).collect()
                },
            }
        }
        Ok(out) => Check {
            level: Level::Fail,
            label: name.into(),
            detail: format!("`{program} {}` exited with {}", args.join(" "), out.status),
        },
        Err(_) => Check {
            level: Level::Fail,
            label: name.into(),
            detail: format!("`{program}` not found - install it first"),
        },
    }
}

async fn check_channel_rights(channels: &[Channel], transport: &dyn Transport) -> Check {
    if channels.is_empty() {
        return Check {
            level: Level::Warn,
            label: "channel rights".into(),
            detail: "no channels configured - add one with `nujijushi channel add`".into(),
        };
    }
    let mut worst = Level::Ok;
    let mut problems: Vec<String> = Vec::new();
    for channel in channels {
        match transport.check_chat(&channel.tg_id).await {
            Ok(status) => {
                if !status.can_post {
                    worst = Level::Fail;
                    problems.push(format!(
                        "{}: bot cannot post ({})",
                        channel.title, status.kind
                    ));
                }
            }
            Err(err) => {
                worst = Level::Fail;
                problems.push(format!("{}: {err}", channel.title));
            }
        }
    }
    Check {
        level: worst,
        label: "channel rights".into(),
        detail: if problems.is_empty() {
            format!("{} channel(s) reachable, bot can post", channels.len())
        } else {
            problems.join("; ")
        },
    }
}

// ---------------------------------------------------------------- import / archive

async fn cmd_import(
    state: &AppState,
    targets: Vec<String>,
    channel: Option<String>,
    priority: i64,
) -> Result<()> {
    let channel = match &channel {
        Some(selector) => Some(resolve_channel(state, selector).await?),
        None => None,
    };

    let targets: Vec<_> = targets
        .iter()
        .flat_map(|target| crate::source::split_pasted_links(target))
        .collect();
    let mut failures = 0;
    for target in targets.iter().copied() {
        let display = crate::source::display_import_target(target);
        println!("importing {display}");
        match crate::jobs::import_and_record(state, target).await {
            Ok(outcome) => {
                println!("  {display}: {}", outcome.summary());
                for (source_id, reason) in &outcome.skipped {
                    println!("  skipped {source_id}: {reason}");
                }
                if let Some(channel) = &channel {
                    for post_id in &outcome.post_ids {
                        match crate::queue::enqueue_checked(state, channel, *post_id, priority)
                            .await
                        {
                            Ok(result) => println!("  post #{post_id} -> {}", result.describe()),
                            Err(err) => println!("  post #{post_id}: {err}"),
                        }
                    }
                }
            }
            Err(err) => {
                failures += 1;
                println!("  failed: {err}");
            }
        }
    }
    if channel.is_some() {
        reputation_notifications::check(state).await;
    }
    if failures == targets.len() {
        return Err(Error::other(format!(
            "all {failures} import target(s) failed"
        )));
    }
    Ok(())
}

async fn cmd_ls(
    state: &AppState,
    filter_state: Option<String>,
    source: Option<String>,
    tag: Option<String>,
    nsfw: Option<String>,
    search: Option<String>,
    limit: i64,
) -> Result<()> {
    let filter = crate::db::posts::PostFilter {
        state: match filter_state.as_deref() {
            Some(raw) => Some(PostState::try_parse(raw).ok_or_else(|| {
                Error::invalid(format!(
                    "unknown state {raw:?} (new, approved, rejected, archived)"
                ))
            })?),
            None => None,
        },
        source,
        tag,
        nsfw: match nsfw.as_deref() {
            Some("yes" | "true" | "1") => Some(true),
            Some("no" | "false" | "0") => Some(false),
            Some(other) => {
                return Err(Error::invalid(format!(
                    "--nsfw expects yes/no, got {other}"
                )));
            }
            None => None,
        },
        search,
        // the CLI lists everything: scripted checks want rejected and sent posts
        sent: crate::db::posts::SentFilter::Any,
        without_rejected: false,
        limit,
        offset: 0,
        author_id: None,
    };
    let cards = state
        .db
        .call(move |conn| crate::db::posts::cards(conn, &filter))
        .await?;
    if cards.is_empty() {
        println!("no posts match");
        return Ok(());
    }
    println!(
        "{:>5}  {:<8} {:<10} {:<20} {:>3} {:>3}  title",
        "id", "state", "rating", "artist", "med", "nsfw"
    );
    for card in &cards {
        println!(
            "{:>5}  {:<8} {:<10} {:<20} {:>3} {:>3}  {}",
            card.id,
            card.state.as_str(),
            card.rating.as_str(),
            card.author_name
                .as_deref()
                .unwrap_or("-")
                .chars()
                .take(20)
                .collect::<String>(),
            card.media_count,
            if card.rating.is_nsfw() { "yes" } else { "no" },
            card.snippet
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(50)
                .collect::<String>()
        );
    }
    println!("\n{} post(s)", cards.len());
    Ok(())
}

async fn cmd_show(state: &AppState, id: i64, channel: Option<String>, raw: bool) -> Result<()> {
    let post = state
        .db
        .call(move |conn| crate::db::posts::get(conn, id))
        .await?;
    // diagnostics for "why is this rated/attributed like that?"
    let (raw_json, submitted_by) = state
        .db
        .call(move |conn| crate::db::posts::diagnostics(conn, id))
        .await?;

    println!("post #{}  {} / {}", post.id, post.source, post.source_id);
    if let Some(url) = &post.url {
        println!("url       {url}");
    }
    println!(
        "artist    {}",
        post.author
            .as_ref()
            .map(|a| a.display())
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "rating    {}  (nsfw override: {})",
        post.rating,
        match post.nsfw_override {
            Some(true) => "yes",
            Some(false) => "no",
            None => "auto",
        }
    );
    println!("state     {}", post.state);
    println!(
        "imported  {}",
        post.imported_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
    );
    if let Some(title) = &post.title {
        println!("title     {title}");
    }
    if let Some(description) = &post.description {
        println!(
            "desc      {}",
            description
                .replace('\n', " ")
                .chars()
                .take(200)
                .collect::<String>()
        );
    }
    if !post.tags.is_empty() {
        println!(
            "tags      {}",
            post.tags
                .iter()
                .map(|t| format!("#{t}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let total_bytes: u64 = post.media.iter().map(|m| m.bytes.max(0) as u64).sum();
    println!(
        "media     {} item(s) · {}",
        post.media.len(),
        crate::model::format_bytes(total_bytes)
    );
    for media in &post.media {
        println!(
            "  #{} {}{} {}x{} {:.1} KiB  {}{}",
            media.ordinal,
            media.ext,
            if media.is_video { " (video)" } else { "" },
            media.width.unwrap_or(0),
            media.height.unwrap_or(0),
            media.bytes as f64 / 1024.0,
            media.blob_hash.chars().take(12).collect::<String>(),
            // the source flagged this item on its own (twitter marks images)
            if media.sensitive { "  nsfw" } else { "" }
        );
    }

    if let Some(tg_id) = submitted_by {
        println!("via       Telegram user {tg_id}");
    }
    if raw {
        println!();
        match raw_json {
            Some(text) => println!("{}", pretty_metadata(&text)),
            None => println!("(no source metadata stored for this post)"),
        }
        // each item keeps its own sidecar: twitter reports per-image fields there
        for media in &post.media {
            if let Some(meta) = &media.meta_json {
                println!(
                    "\n--- media #{} ---\n{}",
                    media.ordinal,
                    pretty_metadata(meta)
                );
            }
        }
    }

    if let Some(selector) = channel {
        let channel = resolve_channel(state, &selector).await?;
        let rules = state
            .db
            .call(|conn| crate::db::tags::TagRules::load(conn))
            .await?;
        let post = state
            .db
            .call(move |conn| crate::db::media_merges::outgoing_post(conn, id))
            .await?;
        let decision = crate::nsfw::evaluate(&post, &channel, &rules);
        let plan = crate::tg::publish::build_plan(state, &channel, &post, 0, decision.clone())?;
        println!(
            "\n--- caption for {} ({}) ---",
            channel.title, channel.tg_id
        );
        println!("{}", plan.caption.text);
        println!(
            "--- {} char(s), {} message(s), spoiler={} ---",
            plan.caption.length,
            plan.messages.len(),
            decision.spoiler()
        );
        println!("policy: {}", plan.decision.reason);
    }
    Ok(())
}

/// Pretty-print stored extractor metadata for `show --raw`, else hand it back as is.
fn pretty_metadata(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

fn cmd_tagger(state: &AppState, cmd: TaggerCmd) -> Result<()> {
    use crate::db::tag_suggestions;

    match cmd {
        TaggerCmd::Map {
            character_id,
            franchise,
            name,
        } => {
            state.db.with(|conn| {
                tag_suggestions::set_mapping(conn, character_id, &franchise, name.as_deref())
            })?;
            println!("saved franchise override for character {character_id}");
        }
        TaggerCmd::Unmap { character_id } => {
            let removed = state
                .db
                .with(|conn| tag_suggestions::remove_mapping(conn, character_id))?;
            if removed {
                println!("removed franchise override for character {character_id}");
            } else {
                println!("no franchise override for character {character_id}");
            }
        }
        TaggerCmd::Mappings => {
            for mapping in state.db.with(|conn| tag_suggestions::mappings(conn))? {
                if let Some(franchise) = mapping.franchise {
                    println!(
                        "{}\t{}\t{}",
                        mapping.character_tag_id, franchise, mapping.origin
                    );
                }
            }
        }
    }
    Ok(())
}

fn cmd_tag(state: &AppState, id: i64, tags: Vec<String>, remove: bool) -> Result<()> {
    if tags.is_empty() {
        return Err(Error::invalid("no tags given"));
    }
    state.db.with(|conn| {
        crate::db::transaction(conn, |conn| {
            if remove {
                crate::db::tags::remove(conn, id, &tags)
            } else {
                crate::db::tags::add_manual(conn, id, &tags)
            }
        })
    })?;
    let verb = if remove { "removed" } else { "added" };
    println!("{verb} {} tag(s) on post #{id}", tags.len());
    Ok(())
}

fn cmd_rate(state: &AppState, id: i64, rating: String) -> Result<()> {
    let rating = rating
        .parse::<Rating>()
        .map_err(|_| Error::invalid("rating must be sfw, questionable or explicit"))?;
    state
        .db
        .with(|conn| crate::db::posts::set_rating(conn, id, rating))?;
    println!("post #{id} is now {rating}");
    Ok(())
}

fn cmd_nsfw(state: &AppState, id: i64, value: String) -> Result<()> {
    let override_value = match value.to_ascii_lowercase().as_str() {
        "auto" | "reset" => None,
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        other => {
            return Err(Error::invalid(format!(
                "expected auto, yes or no, got {other}"
            )));
        }
    };
    state
        .db
        .with(|conn| crate::db::posts::set_nsfw_override(conn, id, override_value))?;
    println!(
        "post #{id} nsfw override: {}",
        match override_value {
            Some(true) => "yes",
            Some(false) => "no",
            None => "auto",
        }
    );
    Ok(())
}

async fn cmd_set_state(
    state: &AppState,
    ids: Vec<i64>,
    target: PostState,
    points: Option<i64>,
) -> Result<()> {
    if points.is_some() && ids.len() != 1 {
        return Err(Error::invalid("--points takes one post ID"));
    }
    let enabled = state.cfg.guest_reputation;
    let count = ids.len();
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            for id in ids {
                crate::db::reputation::review(&tx, id, target, enabled, true, points)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    println!("{count} post(s) -> {target}");
    reputation_notifications::check(state).await;
    Ok(())
}

// ---------------------------------------------------------------- channels

async fn resolve_channel(state: &AppState, selector: &str) -> Result<Channel> {
    let lookup = selector.to_string();
    state
        .db
        .call(move |conn| crate::db::channels::find(conn, &lookup))
        .await?
        .ok_or_else(|| Error::NotFound(format!("channel {selector:?}")))
}

fn channel_line(channel: &Channel, queued: i64, now: DateTime<Utc>) -> String {
    let state = if !channel.enabled {
        "disabled".to_string()
    } else if channel.paused {
        "PAUSED".to_string()
    } else {
        crate::queue::scheduler::slot_reason(channel, now, 0)
    };
    format!(
        "#{:<3} {:<14} {:<22} {:<18} every {:>4}m  nsfw={:<6} queued={:<4} {}",
        channel.id,
        channel.alias.chars().take(14).collect::<String>(),
        channel.title.chars().take(22).collect::<String>(),
        channel.tg_id.chars().take(18).collect::<String>(),
        channel.interval_minutes,
        channel.nsfw_policy.as_str(),
        queued,
        state
    )
}

async fn cmd_channel(state: &AppState, cmd: ChannelCmd) -> Result<()> {
    match cmd {
        ChannelCmd::Add {
            alias,
            chat,
            no_verify,
            opts,
        } => {
            let (alias, chat) = if chat.is_none()
                && opts.tg_id.is_none()
                && alias
                    .as_deref()
                    .is_some_and(|value| value.starts_with('@') || value.parse::<i64>().is_ok())
            {
                (None, alias)
            } else {
                (alias, chat)
            };
            let chat_id = opts
                .tg_id
                .clone()
                .or(chat)
                .filter(|chat| !chat.trim().is_empty())
                .ok_or_else(|| Error::invalid("usage: nujijushi channel add [alias] <@name|id>"))?;
            let mut channel = Channel::new("", chat_id.clone());
            if let Some(alias) = alias.or_else(|| opts.alias.clone()) {
                channel.alias = crate::model::slugify(&alias);
            }
            channel.title = opts.title.clone().unwrap_or_else(|| channel.alias.clone());

            // ask Telegram what this chat is, so typos fail here and not at the first post
            if !no_verify {
                match &state.transport {
                    Some(transport) => match transport.get_chat(&chat_id).await {
                        Ok(info) => {
                            if opts.title.is_none()
                                && let Some(title) = info.title
                            {
                                channel.title = title;
                            }
                            if !info.is_channel {
                                println!(
                                    "note: {} is not a channel (kind: {}), continuing anyway",
                                    channel.title,
                                    if info.is_private {
                                        "private chat"
                                    } else {
                                        "group"
                                    }
                                );
                            }
                        }
                        Err(err) => {
                            return Err(Error::other(format!(
                                "Telegram does not recognise {chat_id}: {err}\n\
                                 fix the id/username, or pass --no-verify to add it anyway"
                            )));
                        }
                    },
                    None => {
                        println!("note: no bot token, skipping the getChat check for {chat_id}")
                    }
                }
            }
            opts.apply(&mut channel)?;
            state.renderer.validate_channel(&channel)?;
            let to_insert = channel.clone();
            let id = state
                .db
                .call(move |conn| crate::db::channels::insert(conn, &to_insert))
                .await?;
            println!("created channel #{id} {}", channel.title);
            println!("  alias   {}", channel.alias);
            println!(
                "  every {}m, active {} - {} ({})",
                channel.interval_minutes, channel.active_from, channel.active_to, channel.tz
            );
            println!(
                "  nsfw policy {}, tags {}, attribution {}",
                channel.nsfw_policy, channel.tag_style, channel.attribution
            );
            Ok(())
        }
        ChannelCmd::Ls => {
            let channels = state
                .db
                .call(|conn| crate::db::channels::list(conn))
                .await?;
            if channels.is_empty() {
                println!("no channels yet - `nujijushi channel add --title ... --tg-id ...`");
                return Ok(());
            }
            let now = Utc::now();
            for channel in &channels {
                let channel_id = channel.id;
                let queued = state
                    .db
                    .call(move |conn| {
                        crate::db::queue::count_state(conn, channel_id, QueueState::Queued)
                    })
                    .await?;
                println!("{}", channel_line(channel, queued, now));
            }
            Ok(())
        }
        ChannelCmd::Edit { channel, opts } => {
            let mut existing = resolve_channel(state, &channel).await?;
            opts.apply(&mut existing)?;
            state.renderer.validate_channel(&existing)?;
            let to_update = existing.clone();
            state
                .db
                .call(move |conn| crate::db::channels::update(conn, &to_update))
                .await?;
            println!("updated channel #{}", existing.id);
            Ok(())
        }
        ChannelCmd::Rm { channel } => {
            let existing = resolve_channel(state, &channel).await?;
            let id = existing.id;
            state
                .db
                .call(move |conn| crate::db::channels::delete(conn, id))
                .await?;
            println!("removed channel #{} {}", existing.id, existing.title);
            Ok(())
        }
        ChannelCmd::Pause { channel } => {
            let existing = resolve_channel(state, &channel).await?;
            let id = existing.id;
            state
                .db
                .call(move |conn| crate::db::channels::set_paused(conn, id, true))
                .await?;
            println!("paused #{} {}", existing.id, existing.title);
            Ok(())
        }
        ChannelCmd::Resume { channel } => {
            let existing = resolve_channel(state, &channel).await?;
            let id = existing.id;
            state
                .db
                .call(move |conn| crate::db::channels::resume(conn, id))
                .await?;
            println!("resumed #{} {}", existing.id, existing.title);
            Ok(())
        }
        ChannelCmd::Discover => {
            let channels = state
                .db
                .call(|conn| crate::db::channels::list_discovered(conn))
                .await?;
            if channels.is_empty() {
                println!("no channels discovered yet.");
                println!(
                    "add the bot as an administrator of a channel while `nujijushi run` is up \
                     and it will show up here."
                );
                return Ok(());
            }
            println!("{:<16} {:<24} {:<22} state", "alias", "title", "chat");
            for channel in &channels {
                println!(
                    "{:<16} {:<24} {:<22} {}",
                    channel.alias,
                    channel.title.chars().take(24).collect::<String>(),
                    channel.tg_id.chars().take(22).collect::<String>(),
                    if channel.enabled {
                        "enabled"
                    } else {
                        "waiting for enable"
                    }
                );
            }
            println!("\nenable one with: nujijushi channel enable <alias>");
            Ok(())
        }
        ChannelCmd::Enable { channel } => set_channel_enabled(state, &channel, true).await,
        ChannelCmd::Disable { channel } => set_channel_enabled(state, &channel, false).await,
    }
}

async fn set_channel_enabled(state: &AppState, selector: &str, enable: bool) -> Result<()> {
    let existing = resolve_channel(state, selector).await?;
    let id = existing.id;
    state
        .db
        .call(move |conn| {
            crate::db::channels::set_enabled(conn, id, enable)?;
            if enable {
                crate::db::channels::set_paused(conn, id, false)?;
            }
            Ok(())
        })
        .await?;
    println!(
        "{} {}{}",
        existing.alias,
        if enable {
            "enabled — the next slot will fire"
        } else {
            "disabled"
        },
        format_args!(" ({})", existing.title)
    );
    Ok(())
}

// ---------------------------------------------------------------- queue

async fn cmd_queue(state: &AppState, cmd: QueueCmd) -> Result<()> {
    match cmd {
        QueueCmd::Add {
            ids,
            channel,
            priority,
        } => {
            let channel = resolve_channel(state, &channel).await?;
            let summary = crate::queue::enqueue_many(state, &channel, &ids, priority).await;
            reputation_notifications::check(state).await;
            for (id, outcome) in &summary.outcomes {
                match outcome {
                    crate::queue::EnqueueOutcome::Queued(queue_id) => {
                        println!("post #{id} -> {} (#{queue_id})", channel.title)
                    }
                    other => println!("post #{id} -> {}", other.describe()),
                }
            }
            if let Some((id, err)) = summary.failed.first() {
                return Err(Error::other(format!("post #{id}: {err}")));
            }
            Ok(())
        }
        QueueCmd::Ls {
            channel,
            state: filter,
            limit,
        } => {
            let channels = match channel {
                Some(selector) => vec![resolve_channel(state, &selector).await?],
                None => {
                    state
                        .db
                        .call(|conn| crate::db::channels::list(conn))
                        .await?
                }
            };
            for channel in channels {
                let states: Vec<QueueState> =
                    match filter.as_deref() {
                        Some(raw) => vec![QueueState::try_parse(raw).ok_or_else(|| {
                            Error::invalid(format!("unknown queue state {raw:?}"))
                        })?],
                        None => vec![QueueState::Queued],
                    };
                let channel_id = channel.id;
                let states_clone = states.clone();
                let entries = state
                    .db
                    .call(move |conn| {
                        crate::db::queue::entries(conn, channel_id, &states_clone, limit)
                    })
                    .await?;
                // the plan the scheduler would follow: which item takes which slot
                let planned: std::collections::HashMap<i64, chrono::DateTime<Utc>> =
                    if states.contains(&QueueState::Queued) {
                        crate::tg::publish::project_queue(state, &channel, entries.len().min(200))
                            .await?
                            .into_iter()
                            .map(|projected| (projected.queue_id, projected.slot))
                            .collect()
                    } else {
                        std::collections::HashMap::new()
                    };
                let now = Utc::now();
                println!(
                    "\n{} ({}) - {} item(s)",
                    channel.title,
                    channel.tg_id,
                    entries.len()
                );
                for entry in &entries {
                    let eta = planned.get(&entry.item.id).map(|slot| {
                        format!(" est. {}", crate::tg::publish::slot_label(*slot, now))
                    });
                    println!(
                        "  q#{:<5} post#{:<6} {:<10} prio={:<3} {:<20} {}{}",
                        entry.item.id,
                        entry.item.post_id,
                        entry.item.state.as_str(),
                        entry.item.priority,
                        entry
                            .item
                            .added_at
                            .with_timezone(&Local)
                            .format("%Y-%m-%d %H:%M"),
                        entry
                            .card
                            .snippet
                            .as_deref()
                            .or(entry.card.author_name.as_deref())
                            .unwrap_or("-")
                            .chars()
                            .take(40)
                            .collect::<String>(),
                        eta.unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        QueueCmd::Skip { id } => {
            state
                .db
                .call(move |conn| crate::db::queue::skip(conn, id))
                .await?;
            println!("queue #{id} skipped");
            Ok(())
        }
        QueueCmd::Pin { id, off } => {
            state
                .db
                .call(move |conn| crate::db::queue::set_pinned(conn, id, !off))
                .await?;
            println!("queue #{id} {}", if off { "unpinned" } else { "pinned" });
            Ok(())
        }
        QueueCmd::Reset { id } => {
            state
                .db
                .call(move |conn| crate::db::queue::requeue(conn, id))
                .await?;
            println!("queue #{id} back in the queue");
            Ok(())
        }
        QueueCmd::Rm { id } => {
            let removed = state
                .db
                .call(move |conn| crate::db::queue::delete(conn, id))
                .await?;
            if removed {
                println!("removed queue #{id}");
            } else {
                return Err(Error::NotFound(format!("queue item {id}")));
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- publish

async fn cmd_preview(state: &AppState, channel: String, count: usize) -> Result<()> {
    let channel = resolve_channel(state, &channel).await?;
    let items = crate::tg::publish::preview_channel(state, &channel, count).await?;
    if items.is_empty() {
        println!(
            "nothing to preview for {} - the queue is empty or every item is vetoed",
            channel.title
        );
        return Ok(());
    }
    println!(
        "{} ({}) - next {} slot(s), every {}m\n",
        channel.title,
        channel.tg_id,
        items.len(),
        channel.interval_minutes
    );
    let now = Utc::now();
    for (index, item) in items.iter().enumerate() {
        println!(
            "=== slot {} at {} (in {}) | q#{} post#{} | {} media, {} message(s) | {}{} ===",
            index + 1,
            crate::tg::publish::slot_label(item.eta, now),
            crate::tg::publish::format_duration(item.eta - now),
            item.queue_id,
            item.post_id,
            item.media,
            item.messages,
            item.reason,
            if item.spoiler { " | spoiler" } else { "" }
        );
        println!("{}", item.caption);
        println!();
    }
    Ok(())
}

async fn cmd_push(state: &AppState, channel: String, dry_run: bool, force: bool) -> Result<()> {
    let channel = resolve_channel(state, &channel).await?;
    let outcome = crate::tg::publish::publish_channel(state, &channel, dry_run, force).await;
    if !dry_run {
        queue_alerts::check(state).await;
    }
    println!("{}", outcome?.describe());
    Ok(())
}

async fn cmd_bot(state: &AppState, once: bool) -> Result<()> {
    if !once {
        // Standalone polling also publishes via /publish; repair stale rows too.
        crate::jobs::worker::reconcile_startup(state).await?;
    }
    crate::bot::Bot::new(state).await?.run(once).await?;
    if state.cfg.bot_mode == crate::config::BotMode::Webhook && !once {
        crate::web::serve(state.clone(), None).await?;
    }
    Ok(())
}

async fn cmd_run(
    state: &AppState,
    once: bool,
    tick: u64,
    no_web: bool,
    no_bot: bool,
    bind: Option<String>,
) -> Result<()> {
    if no_web && state.cfg.bot_mode == crate::config::BotMode::Webhook {
        return Err(Error::config(
            "webhook mode needs the web server (Telegram POSTs to /tg/webhook): \
             drop --no-web or set [bot] mode = \"polling\"",
        ));
    }
    crate::jobs::worker::reconcile_startup(state).await?;

    if !no_web {
        let web_state = state.clone();
        let bind = bind.clone();
        tokio::spawn(async move {
            if let Err(err) = crate::web::serve(web_state, bind).await {
                tracing::error!("admin UI stopped: {err}");
            }
        });
    }

    if !no_bot && state.cfg.bot_enabled {
        if state.transport.is_some() {
            let bot_state = state.clone();
            tokio::spawn(async move {
                match crate::bot::Bot::new(&bot_state).await {
                    Ok(bot) => {
                        if let Err(err) = bot.run(false).await {
                            tracing::error!("bot interface stopped: {err}");
                        }
                    }
                    Err(err) => tracing::error!("cannot start the bot interface: {err}"),
                }
            });
        } else {
            tracing::warn!("bot interface disabled: no TG_BOT_TOKEN");
        }
    }

    // `serve` owns the worker when the UI is up; otherwise `run` must do it
    if no_web {
        let worker = crate::jobs::worker::JobWorker::new(state.clone());
        tokio::spawn(async move {
            if let Err(err) = worker.run(std::future::pending::<()>()).await {
                tracing::error!("job worker stopped: {err}");
            }
        });
    }

    let tick = std::time::Duration::from_secs(tick.max(1));
    loop {
        scheduler_pass(state).await?;
        if once {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nstopping");
                return Ok(());
            }
            _ = tokio::time::sleep(tick) => {}
        }
    }
}

async fn scheduler_pass(state: &AppState) -> Result<()> {
    let channels = state
        .db
        .call(|conn| crate::db::channels::list(conn))
        .await?;
    let now = Utc::now();
    // heartbeat for the UI: "nothing due" and "nothing running" look identical
    // from outside, and that is exactly the question a silent queue raises
    if let Err(err) = state
        .db
        .set_setting(crate::queue::scheduler::TICK_KEY, &now.to_rfc3339())
    {
        tracing::warn!("cannot record the scheduler heartbeat: {err}");
    }
    for channel in channels {
        if !channel.enabled || channel.paused {
            continue;
        }
        if !crate::queue::scheduler::is_due(&channel, now) {
            continue;
        }
        match crate::tg::publish::publish_channel(state, &channel, false, false).await {
            Ok(outcome) => {
                let text = outcome.describe();
                match outcome {
                    crate::tg::publish::PublishOutcome::NotDue(_) => {}
                    _ => println!("[{}] {text}", channel.title),
                }
            }
            Err(err) => {
                tracing::error!(channel = %channel.title, "publish failed: {err}");
            }
        }
    }
    Ok(())
}

async fn cmd_log(state: &AppState, limit: i64) -> Result<()> {
    let entries = crate::tg::audit::recent(state, limit).await?;
    if entries.is_empty() {
        println!("no sends yet");
        return Ok(());
    }
    for entry in &entries {
        println!(
            "{}  channel#{} post#{:<6} {:<5} {}{}",
            entry.sent_at.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
            entry.channel_id,
            entry.post_id.unwrap_or(0),
            if entry.ok { "ok" } else { "FAIL" },
            entry.tg_message_ids.clone().unwrap_or_default(),
            entry
                .error
                .as_ref()
                .map(|e| format!("  {e}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- maintenance

async fn cmd_gc(
    cfg: Config,
    dry_run: bool,
    sent: bool,
    older_than_days: Option<u32>,
    yes: bool,
) -> Result<()> {
    use crate::storage::cleanup;
    let state = AppState::open(cfg, None)?;
    if sent {
        let days = older_than_days.unwrap_or(cleanup::DEFAULT_AGE_DAYS);
        if yes {
            let report = cleanup::run(&state, cleanup::cutoff(days)?, None, None).await?;
            println!("{}", report.describe());
            if report.files.failed_files > 0 {
                return Err(Error::other(
                    "Some archive files could not be removed. Run gc to retry.",
                ));
            }
        } else {
            println!("{}", cleanup::preview(&state, days).await?.describe());
        }
        return Ok(());
    }
    if dry_run {
        let (blobs, bytes) = state
            .db
            .with(|conn| crate::db::media_merges::orphan_blob_totals(conn))?;
        println!(
            "would delete {blobs} unreferenced blob(s), {:.1} MiB (dry run)",
            bytes as f64 / 1_048_576.0
        );
        return Ok(());
    }
    let report = cleanup::collect_orphans(&state).await?;
    println!(
        "removed {} blob(s) and {} thumbnail(s), freed {:.1} MiB",
        report.removed_blobs,
        report.removed_thumbs,
        report.freed_bytes as f64 / 1_048_576.0
    );
    if report.failed_files > 0 {
        return Err(Error::other(format!(
            "{} file(s) could not be removed. Run gc to retry.",
            report.failed_files
        )));
    }
    Ok(())
}

async fn cmd_export(cfg: Config, out: Option<PathBuf>, with_media: bool) -> Result<()> {
    let state = AppState::open(cfg, None)?;
    let _storage = if with_media {
        Some(state.storage_read().await?)
    } else {
        None
    };
    let db = &state.db;
    let cfg = &state.cfg;
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = out.unwrap_or_else(|| cfg.export_dir().join(format!("backup-{stamp}")));
    std::fs::create_dir_all(&dir)?;

    // consistent database snapshot
    let db_target = dir.join("nujijushi.sqlite3");
    {
        let target = db_target.display().to_string();
        db.with(|conn| {
            conn.execute("VACUUM INTO ?1", [target])?;
            Ok(())
        })?;
    }

    // metadata as JSONL, plus a channels file
    let posts = db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.id, p.source, p.source_id, p.url, p.title, p.description, p.rating, p.state,
                    a.name AS author, a.handle AS handle
             FROM posts p LEFT JOIN authors a ON a.id = p.author_id ORDER BY p.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "source": row.get::<_, String>(1)?,
                "source_id": row.get::<_, String>(2)?,
                "url": row.get::<_, Option<String>>(3)?,
                "title": row.get::<_, Option<String>>(4)?,
                "description": row.get::<_, Option<String>>(5)?,
                "rating": row.get::<_, String>(6)?,
                "state": row.get::<_, String>(7)?,
                "author": row.get::<_, Option<String>>(8)?,
                "handle": row.get::<_, Option<String>>(9)?,
            }))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut lines = String::new();
    for post in &posts {
        lines.push_str(&post.to_string());
        lines.push('\n');
    }
    std::fs::write(dir.join("posts.jsonl"), lines)?;

    let channels = db.with(|conn| crate::db::channels::list(conn))?;
    let channel_json = serde_json::json!(
        channels
            .iter()
            .map(|c| serde_json::json!({
                "id": c.id,
                "title": c.title,
                "tg_id": c.tg_id,
                "interval_minutes": c.interval_minutes,
                "active_from": c.active_from,
                "active_to": c.active_to,
                "tz": c.tz,
                "nsfw_policy": c.nsfw_policy.as_str(),
                "tag_style": c.tag_style.as_str(),
                "attribution": c.attribution.as_str(),
                "template_name": c.template_name,
                "template": c.template,
            }))
            .collect::<Vec<_>>()
    );
    std::fs::write(
        dir.join("channels.json"),
        serde_json::to_string_pretty(&channel_json)?,
    )?;

    let mut media_copied = 0;
    if with_media {
        let dest = dir.join("blobs");
        let store = crate::blob::BlobStore::new(cfg.blobs_dir(), cfg.thumbs_dir())?;
        media_copied = copy_dir(store.root(), &dest)?;
        copy_dir(&cfg.thumbs_dir(), &dir.join("thumbs"))?;
    }

    println!("backup written to {}", dir.display());
    println!("  database  {}", db_target.display());
    println!(
        "  metadata  posts.jsonl ({} post(s)), channels.json",
        posts.len()
    );
    if with_media {
        println!("  media     {media_copied} blob file(s)");
    }
    Ok(())
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> Result<usize> {
    let mut copied = 0;
    if !from.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(from)?.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copied += copy_dir(&path, &target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &target)?;
            copied += 1;
        }
    }
    Ok(copied)
}

async fn cmd_alias(state: &AppState, cmd: AliasCmd) -> Result<()> {
    match cmd {
        AliasCmd::Set { from, to } => {
            let (f, t) = (from.clone(), to.clone());
            state
                .db
                .call(move |conn| crate::db::tags::set_alias(conn, &f, &t))
                .await?;
            println!("alias set: {from} -> {to} (applies to future imports)");
            Ok(())
        }
    }
}

async fn cmd_rule(state: &AppState, cmd: RuleCmd) -> Result<()> {
    match cmd {
        RuleCmd::Add { tag, action } => {
            let action = match action.to_ascii_lowercase().as_str() {
                "block" => crate::model::TagRuleAction::Block,
                "nsfw" => crate::model::TagRuleAction::Nsfw,
                other => {
                    return Err(Error::invalid(format!(
                        "unknown rule action {other:?} (block or nsfw)"
                    )));
                }
            };
            let tag_for_db = tag.clone();
            state
                .db
                .call(move |conn| crate::db::tags::set_rule(conn, &tag_for_db, action))
                .await?;
            println!("rule set: {tag} -> {}", action.as_str());
            Ok(())
        }
        RuleCmd::Ls => {
            let rules = state
                .db
                .call(|conn| {
                    let mut stmt =
                        conn.prepare("SELECT tag, action FROM tag_rules ORDER BY action, tag")?;
                    let rows = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
                    }
                    Ok(out)
                })
                .await?;
            if rules.is_empty() {
                println!("no rules - `nujijushi rule add guro block`");
                return Ok(());
            }
            for (tag, action) in rules {
                println!("  {action:<5} {tag}");
            }
            Ok(())
        }
        RuleCmd::Rm { tag } => {
            let removed = state
                .db
                .call(move |conn| crate::db::tags::remove_rule(conn, &tag))
                .await?;
            println!(
                "{}",
                if removed {
                    "rule removed"
                } else {
                    "no such rule"
                }
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- users

async fn cmd_users(state: &AppState, cmd: UsersCmd) -> Result<()> {
    match cmd {
        UsersCmd::Ls => {
            let users = state
                .db
                .call(|conn| crate::db::users::list(conn, 100))
                .await?;
            if users.is_empty() {
                println!(
                    "no users recorded yet - they appear after their first message to the bot"
                );
                return Ok(());
            }
            println!("{:<14} {:<8} {:<22} last seen", "tg id", "role", "user");
            for user in &users {
                let config_admin = state.cfg.access.is_admin(user.tg_id);
                // the role the bot will actually use, not the raw row: a config
                // admin who messaged once has a `guest` row and used to be listed
                // as a guest, which reads like a broken config
                let role = crate::bot::access::display_role(config_admin, &user.role);
                let note = match (config_admin, user.role.as_str()) {
                    (true, "admin") => " (config admin)".to_string(),
                    (true, other) => format!(" (config admin; db: {other})"),
                    (false, _) => String::new(),
                };
                println!(
                    "{:<14} {:<8} {:<22} {}{}",
                    user.tg_id,
                    role,
                    user.label().chars().take(22).collect::<String>(),
                    user.last_seen_at
                        .as_deref()
                        .and_then(crate::db::parse_ts)
                        .map(|ts| ts.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    note
                );
            }
            Ok(())
        }
        UsersCmd::Grant { tg_id, role } => {
            if !matches!(role.as_str(), "admin" | "guest" | "banned") {
                return Err(Error::invalid("role must be admin, guest or banned"));
            }
            if role != "admin" && state.cfg.access.is_admin(tg_id) {
                return Err(Error::invalid(
                    "that id is an admin in config.toml - edit the config to demote them",
                ));
            }
            let role_owned = role.clone();
            state
                .db
                .call(move |conn| crate::db::users::set_role(conn, tg_id, &role_owned))
                .await?;
            crate::bot::menus::sync_stored_user(state, tg_id).await;
            println!("{tg_id} -> {role}");
            Ok(())
        }
        UsersCmd::Ban { tg_id } => {
            if state.cfg.access.is_admin(tg_id) {
                return Err(Error::invalid(
                    "that id is an admin in config.toml - edit the config to ban them",
                ));
            }
            state
                .db
                .call(move |conn| crate::db::users::set_role(conn, tg_id, "banned"))
                .await?;
            crate::bot::menus::sync_stored_user(state, tg_id).await;
            println!("{tg_id} banned");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- storage

async fn cmd_storage(state: &AppState, cmd: StorageCmd) -> Result<()> {
    let sinks = crate::storage::sinks(state);
    let totals = crate::storage::summary(state).unwrap_or_default();

    let counts = |sink: &str, wanted: &str| -> i64 {
        totals
            .iter()
            .filter(|(name, state, _)| name == sink && state == wanted)
            .map(|(_, _, count)| *count)
            .sum()
    };

    match cmd {
        StorageCmd::Ls => {
            if sinks.is_empty() {
                println!("no storage sink enabled - see [storage] in config.toml");
                println!("  [storage.local] enabled = true / path = \"/path/to/folder\"");
                println!("  [storage.webdav] enabled = true / url = \"https://…\"");
                println!("  [storage.telegram] enabled = true / chat_id = \"-100…\"");
                return Ok(());
            }
            println!(
                "{:<10} {:<44} {:>6} {:>8} {:>6}",
                "sink", "where", "done", "pending", "failed"
            );
            for sink in &sinks {
                println!(
                    "{:<10} {:<44} {:>6} {:>8} {:>6}",
                    sink.name(),
                    sink.describe().chars().take(44).collect::<String>(),
                    counts(sink.name(), "done"),
                    counts(sink.name(), "pending"),
                    counts(sink.name(), "failed"),
                );
            }
            let done: i64 = totals
                .iter()
                .filter(|(_, state, _)| state == "done")
                .map(|(_, _, count)| *count)
                .sum();
            println!(
                "\n{done} mirror(s) complete · {} sink(s) enabled",
                sinks.len()
            );
            println!("`nujijushi storage sync` uploads everything still missing");
            Ok(())
        }
        StorageCmd::Test => {
            if sinks.is_empty() {
                println!("no storage sink enabled");
                return Ok(());
            }
            let mut failures = 0;
            for sink in &sinks {
                match sink.check().await {
                    Ok(detail) => println!("  [ok  ] {:<10} {detail}", sink.name()),
                    Err(err) => {
                        failures += 1;
                        println!("  [FAIL] {:<10} {err}", sink.name());
                    }
                }
            }
            if failures > 0 {
                return Err(Error::other(format!("{failures} storage sink(s) failed")));
            }
            Ok(())
        }
        StorageCmd::Sync {
            sink,
            post,
            limit,
            dry_run,
        } => {
            if sinks.is_empty() {
                println!("no storage sink enabled");
                return Ok(());
            }
            if let Some(name) = &sink
                && !sinks.iter().any(|s| s.name() == name)
            {
                return Err(Error::invalid(format!(
                    "no such sink: {name} (enabled: {})",
                    sinks
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            if dry_run {
                let pending_limit = limit;
                let pending = state
                    .db
                    .call(move |conn| crate::db::mirrors::pending(conn, None, pending_limit))
                    .await?;
                println!(
                    "would upload {} pending mirror(s) to {}",
                    pending.len(),
                    sinks
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return Ok(());
            }
            let summary = crate::storage::sync(state, post, limit, false).await?;
            println!(
                "queued {} · uploaded {} · {} post(s) considered",
                summary.queued, summary.uploaded, limit
            );
            let totals = crate::storage::summary(state)?;
            for (name, state_name, count) in totals {
                if state_name == "failed" {
                    println!("  {name}: {count} failed mirror(s) - see the dashboard or the logs");
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Args {
        let mut full = vec!["nujijushi"];
        full.extend_from_slice(args);
        Args::try_parse_from(full).expect("the CLI definition must accept this")
    }

    #[test]
    fn flush_accepts_a_channel() {
        assert!(matches!(
            parse(&["flush", "--channel", "art"]).cmd,
            Command::Flush { channel } if channel == "art"
        ));
    }

    #[test]
    fn approve_accepts_a_custom_guest_reward() {
        assert!(matches!(
            parse(&["approve", "--points", "15", "42"]).cmd,
            Command::Approve { ids, points: Some(15) } if ids == [42]
        ));
    }

    #[test]
    fn tagger_catalog_commands_accept_numeric_character_ids() {
        assert!(matches!(
            parse(&["tagger", "map", "3861", "pokemon"]).cmd,
            Command::Tagger {
                cmd: TaggerCmd::Map {
                    character_id: 3861,
                    ..
                }
            },
        ));
        assert!(matches!(
            parse(&["tagger", "unmap", "3861"]).cmd,
            Command::Tagger {
                cmd: TaggerCmd::Unmap { character_id: 3861 }
            },
        ));
        assert!(matches!(
            parse(&["tagger", "map", "3861", "pokemon", "--name", "pikachu"]).cmd,
            Command::Tagger {
                cmd: TaggerCmd::Map { name: Some(name), .. }
            } if name == "pikachu"
        ));
        assert!(matches!(
            parse(&["tagger", "mappings"]).cmd,
            Command::Tagger {
                cmd: TaggerCmd::Mappings
            },
        ));
        assert!(Args::try_parse_from(["nujijushi", "tagger", "map", "0", "pokemon"]).is_err());
    }

    #[test]
    fn channel_add_takes_an_alias_and_a_chat() {
        let args = parse(&[
            "channel",
            "add",
            "night-shift",
            "@night_art",
            "--interval",
            "90",
        ]);
        match args.cmd {
            Command::Channel {
                cmd: ChannelCmd::Add {
                    alias, chat, opts, ..
                },
            } => {
                assert_eq!(alias.as_deref(), Some("night-shift"));
                assert_eq!(chat.as_deref(), Some("@night_art"));
                assert_eq!(opts.interval, Some(90));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn channel_add_still_accepts_flags_only() {
        let args = parse(&["channel", "add", "--title", "X", "--tg-id", "@x"]);
        match args.cmd {
            Command::Channel {
                cmd: ChannelCmd::Add { alias, chat, .. },
            } => {
                assert!(alias.is_none());
                assert!(chat.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn channel_add_accepts_negative_ids_without_an_argument_separator() {
        let id = "-1001234567890";
        for (argv, expected_alias, expected_chat, expected_option) in [
            (
                vec!["channel", "add", "art", id, "--no-verify"],
                Some("art"),
                Some(id),
                None,
            ),
            (
                vec!["channel", "add", "--tg-id", id, "--no-verify"],
                None,
                None,
                Some(id),
            ),
            (
                vec!["channel", "add", id, "--no-verify"],
                Some(id),
                None,
                None,
            ),
        ] {
            let Command::Channel {
                cmd:
                    ChannelCmd::Add {
                        alias,
                        chat,
                        no_verify,
                        opts,
                    },
            } = parse(&argv).cmd
            else {
                panic!("expected channel add");
            };
            assert_eq!(alias.as_deref(), expected_alias);
            assert_eq!(chat.as_deref(), expected_chat);
            assert_eq!(opts.tg_id.as_deref(), expected_option);
            assert!(no_verify);
        }
    }

    #[test]
    fn channel_edit_can_rename_the_alias() {
        let args = parse(&["channel", "edit", "night", "--alias", "night-shift"]);
        match args.cmd {
            Command::Channel {
                cmd: ChannelCmd::Edit { channel, opts },
            } => {
                assert_eq!(channel, "night");
                assert_eq!(opts.alias.as_deref(), Some("night-shift"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn new_commands_parse() {
        assert!(matches!(
            parse(&["bot", "--once"]).cmd,
            Command::Bot { once: true }
        ));
        assert!(matches!(
            parse(&["channel", "discover"]).cmd,
            Command::Channel {
                cmd: ChannelCmd::Discover
            }
        ));
        match parse(&["run", "--once", "--no-bot", "--no-web"]).cmd {
            Command::Run {
                no_bot,
                no_web,
                once,
                ..
            } => {
                assert!(no_bot && no_web && once);
            }
            other => panic!("unexpected command: {other:?}"),
        }
        match parse(&["users", "grant", "42", "admin"]).cmd {
            Command::Users {
                cmd: UsersCmd::Grant { tg_id, role },
            } => {
                assert_eq!(tg_id, 42);
                assert_eq!(role, "admin");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn sent_cleanup_accepts_an_explicit_age_and_apply_flag() {
        assert!(matches!(
            parse(&["gc", "--sent", "--older-than-days", "0", "--yes"]).cmd,
            Command::Gc {
                sent: true,
                older_than_days: Some(0),
                yes: true,
                dry_run: false
            }
        ));
        assert!(Args::try_parse_from(["nujijushi", "gc", "--yes"]).is_err());
        assert!(Args::try_parse_from(["nujijushi", "gc", "--sent", "--yes", "--dry-run"]).is_err());
    }

    #[test]
    fn import_accepts_telegram_links() {
        let args = parse(&["import", "https://t.me/night_art/42"]);
        match args.cmd {
            Command::Import { targets, .. } => {
                assert_eq!(targets, vec!["https://t.me/night_art/42"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

#[cfg(test)]
mod init_tests {
    use super::displayed_ui_token;

    #[test]
    fn init_only_prints_the_token_it_actually_saved() {
        assert_eq!(
            displayed_ui_token(true, None, Some("generated")),
            "generated"
        );
        assert_eq!(
            displayed_ui_token(true, Some("old"), Some("new")),
            "new",
            "a rewritten config gets the token it was written with"
        );
        assert_eq!(
            displayed_ui_token(false, Some("existing"), Some("generated")),
            "existing",
            "an existing config keeps its own token, not a throwaway one"
        );
        assert!(
            displayed_ui_token(false, None, Some("generated")).contains("not set"),
            "an existing config without a token must say so instead of printing a unusable value"
        );
    }
}
