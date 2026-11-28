//! Command handlers. Everything the bot can do is one method here.

use crate::bot::Bot;
use crate::bot::access::{Access, Perm, QuotaKind, Role};
use crate::bot::commands::{self, Parsed, Requirement};
use crate::bot::reply;
use crate::db::{channels as channel_db, posts as post_db, queue as queue_db, users as user_db};
use crate::error::Result;
use crate::model::{Channel, PostState, QueueState, Rating};
use crate::source::telegram;
use crate::tg::botapi::{BotMessage, BotUpdate, BotUser, MemberChange, UpdateKind};

impl<'a> Bot<'a> {
    pub async fn handle(&self, update: BotUpdate) -> Result<()> {
        match update.kind {
            UpdateKind::Message(message) => self.handle_message(message).await,
            UpdateKind::MemberChange(change) => self.handle_member_change(change).await,
            UpdateKind::Other => {
                // channel posts, edits and callback queries all land here
                tracing::debug!("ignoring an update kind the bot does not handle");
                Ok(())
            }
        }
    }

    async fn handle_message(&self, message: BotMessage) -> Result<()> {
        let Some(from) = message.from.clone() else {
            return Ok(());
        };
        if from.is_bot {
            return Ok(());
        }
        // the interface is a private conversation; group chatter is ignored
        if !message.chat_is_private {
            tracing::debug!("ignoring a non-private chat update (commands only work in a DM)");
            return Ok(());
        }

        let role = self.access.resolve(self.state, &from).await?;
        super::menus::sync_user(self.state, message.chat_id, role).await;
        if role == Role::Banned {
            return self.reply(&message, &not_allowed(&from)).await;
        }

        // a forwarded channel post: the media is already in this update
        if message.forward_from_chat_id.is_some() && !message.files.is_empty() {
            return self.import_forwarded(&message, &from, role).await;
        }

        let body = message.body().unwrap_or_default().to_string();
        match commands::parse(&body, self.username.as_deref()) {
            Parsed::Empty => {
                tracing::debug!("ignoring a message without text (nothing to dispatch)");
                Ok(())
            }
            Parsed::Command(command) => self.dispatch(&message, &from, role, command).await,
            Parsed::Text(text) => {
                let targets = commands::import_targets(&text);
                if targets.is_empty() {
                    let text = if self.access.allows(role, Perm::Import) {
                        "Send me a link to archive, or /help for the command list."
                    } else {
                        "Use /help for the command list."
                    };
                    self.reply(&message, text).await
                } else if self.allow(&message, role, Perm::Import).await? {
                    self.import_link(&message, &from, role, targets).await
                } else {
                    Ok(())
                }
            }
        }
    }

    async fn dispatch(
        &self,
        message: &BotMessage,
        user: &BotUser,
        role: Role,
        command: commands::Command,
    ) -> Result<()> {
        let name = command.name.as_str();
        let Some(spec) = commands::MENU.iter().find(|spec| spec.name == name) else {
            return self
                .reply(
                    message,
                    &format!(
                        "Unknown command <code>/{}</code>. Try /help.",
                        reply::esc(name)
                    ),
                )
                .await;
        };
        if !spec.available_to(&self.access, role) {
            return match spec.requirement {
                Requirement::Permission(perm) => self.deny(message, role, perm).await,
                Requirement::Admin => {
                    self.reply(message, "That command is for the bot admin.")
                        .await
                }
                Requirement::AllowedUser => self.reply(message, &not_allowed(user)).await,
            };
        }

        match name {
            "start" => {
                self.reply(message, &start_text(&self.access, user, role))
                    .await
            }
            "help" => self.reply(message, &help_text(&self.access, role)).await,
            "whoami" => {
                let reputation = if self.state.cfg.guest_reputation {
                    if role.is_admin() {
                        Some("∞".to_string())
                    } else {
                        let user_id = user.id;
                        let balance = self
                            .state
                            .db
                            .call(move |conn| crate::db::reputation::balance(conn, user_id))
                            .await?;
                        Some(crate::db::reputation::format_points(balance))
                    }
                } else {
                    None
                };
                let mut text = whoami_text(user, role);
                if let Some(reputation) = reputation {
                    text.push_str(&format!("\nReputation: {reputation}"));
                }
                self.reply(message, &text).await
            }
            "status" => self.status(message).await,
            "channels" => self.channels(message).await,
            "queue" => self.queue(message, command.arg(0)).await,
            "ls" => self.list_posts(message, role, command.arg(0)).await,
            "show" => self.show_post(message, command.arg(0)).await,
            "import" => {
                let targets: Vec<String> = crate::source::split_pasted_links(command.rest())
                    .into_iter()
                    .flat_map(str::split_whitespace)
                    .map(crate::source::normalize_target)
                    .collect();
                self.import_link(message, user, role, targets).await
            }
            "push" => {
                self.push(message, user, role, command.arg(0), command.arg(1))
                    .await
            }
            "publish" => self.publish(message, command.arg(0)).await,
            "approve" | "reject" => {
                let state = if name == "approve" {
                    PostState::Approved
                } else {
                    PostState::Rejected
                };
                self.set_state(message, role, &command.args, state).await
            }
            "nsfw" => self.set_nsfw(message, command.arg(0), command.arg(1)).await,
            "rate" => {
                self.set_rating(message, command.arg(0), command.arg(1))
                    .await
            }
            "tag" => {
                self.set_tags(
                    message,
                    command.arg(0),
                    command.args.get(1..).unwrap_or_default(),
                )
                .await
            }
            "pause" | "resume" => {
                self.toggle_pause(message, command.arg(0), name == "pause")
                    .await
            }
            "enable" | "disable" => {
                self.toggle_enabled(message, command.arg(0), name == "enable")
                    .await
            }
            "discover" => self.discover(message).await,
            "users" => self.users(message).await,
            "grant" | "ban" => {
                let target = command.arg(0);
                let role_arg = if name == "ban" {
                    Some("banned")
                } else {
                    command.arg(1)
                };
                self.grant(message, target, role_arg).await
            }
            _ => Err(crate::error::Error::other(format!(
                "missing handler for /{name}"
            ))),
        }
    }

    // ---------------------------------------------------------------- replies

    pub async fn reply(&self, message: &BotMessage, html: &str) -> Result<()> {
        reply::send(self.transport.as_ref(), &message.chat_id.to_string(), html).await
    }

    pub async fn reply_to(&self, chat_id: i64, html: &str) -> Result<()> {
        reply::send(self.transport.as_ref(), &chat_id.to_string(), html).await
    }

    async fn deny(&self, message: &BotMessage, role: Role, perm: Perm) -> Result<()> {
        // publish is the one gate with two different fixes, so name the right one
        let text = match (perm, role.is_admin()) {
            (Perm::Publish, false) => format!(
                "Publishing from Telegram belongs to the bot owner.\n\
                 Your id is <code>{}</code> — an admin adds it to <code>[access] admins</code> \
                 and sets <code>allow_publish = true</code>.\n\
                 Config file: <code>{}</code>",
                message
                    .from
                    .as_ref()
                    .map(|user| user.id)
                    .unwrap_or_default(),
                self.state.cfg.home.join("config.toml").display()
            ),
            (Perm::Publish, true) => format!(
                "Publishing is switched off: set <code>[access] allow_publish = true</code> \
                 in <code>{}</code> and restart.",
                self.state.cfg.home.join("config.toml").display()
            ),
            _ => format!(
                "You are not allowed to use <code>{}</code> here.",
                perm.as_str()
            ),
        };
        self.reply(message, &text).await
    }

    async fn allow(&self, message: &BotMessage, role: Role, perm: Perm) -> Result<bool> {
        if self.access.allows(role, perm) {
            return Ok(true);
        }
        self.deny(message, role, perm).await?;
        Ok(false)
    }

    // ---------------------------------------------------------------- discovery

    async fn handle_member_change(&self, change: MemberChange) -> Result<()> {
        if !change.is_channel || !change.became_admin {
            return Ok(());
        }
        let tg_id = change.chat_id.to_string();
        let (title, username) = (change.title.clone(), change.username.clone());
        let (channel_id, created) = self
            .state
            .db
            .call({
                let tg_id = tg_id.clone();
                move |conn| channel_db::upsert_discovered(conn, &tg_id, &title, username.as_deref())
            })
            .await?;

        let channel = self
            .state
            .db
            .call(move |conn| channel_db::get(conn, channel_id))
            .await?;

        tracing::info!(
            channel = channel_id,
            created,
            alias = %channel.alias,
            "channel discovered"
        );

        let note = if created {
            format!(
                "Added to <b>{}</b>.\nRegistered as <code>{}</code> (disabled).\n\
                 Enable it from the admin account with /enable {} — or ignore this.",
                reply::esc(&change.title),
                reply::esc(&channel.alias),
                reply::esc(&channel.alias)
            )
        } else {
            format!(
                "Added to <b>{}</b>, which is already known as <code>{}</code>.",
                reply::esc(&change.title),
                reply::esc(&channel.alias)
            )
        };
        for admin in self.access.admins() {
            if let Err(err) = self.reply_to(*admin, &note).await {
                tracing::warn!(
                    admin,
                    "cannot notify admin about a discovered channel: {err}"
                );
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- imports

    async fn import_link(
        &self,
        message: &BotMessage,
        user: &BotUser,
        role: Role,
        targets: Vec<String>,
    ) -> Result<()> {
        // a message may carry a whole batch; cap it so nobody queues 500 jobs
        const MAX_TARGETS: usize = 20;

        if targets.is_empty() {
            return self
                .reply(message, "Usage: <code>/import &lt;link&gt;</code>")
                .await;
        }
        let limit = self.access.config().import_quota_per_day;
        // one quota unit per link, so a batch cannot slip past the daily cap
        let mut queued: Vec<String> = Vec::new();
        let mut hit_quota = false;
        for target in targets.iter().take(MAX_TARGETS) {
            if !self
                .access
                .spend(self.state, role, user.id, QuotaKind::Import)
                .await?
            {
                hit_quota = true;
                break;
            }
            if let Err(err) = crate::jobs::create_job_for(self.state, target, Some(user.id)).await {
                let _ = self
                    .access
                    .refund(self.state, user.id, QuotaKind::Import)
                    .await;
                return Err(err);
            }
            queued.push(target.clone());
        }
        if queued.is_empty() {
            let used = self
                .access
                .used_today(self.state, user.id, QuotaKind::Import)
                .await?;
            return self
                .reply(
                    message,
                    &format!("Daily import limit reached ({used}/{limit})."),
                )
                .await;
        }

        let mut body = format!(
            "Importing {} link(s).\nI will message you as each one finishes.\n<pre>{}</pre>",
            queued.len(),
            reply::esc(&queued.join("\n"))
        );
        if hit_quota {
            let used = self
                .access
                .used_today(self.state, user.id, QuotaKind::Import)
                .await?;
            body.push_str(&format!(
                "\n\nDaily import limit reached ({used}/{limit}): the rest was skipped."
            ));
        }
        if targets.len() > MAX_TARGETS {
            body.push_str(&format!(
                "\n\nOnly the first {MAX_TARGETS} of {} links were taken.",
                targets.len()
            ));
        }
        // guests are not silent: their links land in the archive without anyone
        // seeing them, so the admins get a line per batch (never the submitter)
        if role != Role::Admin {
            notify_admins_of_submission(self.state, user, &queued).await;
        }
        self.reply(message, &body).await
    }

    async fn import_forwarded(
        &self,
        message: &BotMessage,
        user: &BotUser,
        role: Role,
    ) -> Result<()> {
        if !self.allow(message, role, Perm::Import).await? {
            return Ok(());
        }
        if let (Some(chat), Some(id)) = (
            message.forward_from_chat_id,
            message.forward_from_message_id,
        ) {
            let mut targets = vec![chat.to_string()];
            if let Some(username) = &message.forward_from_chat_username {
                targets.push(format!("@{username}"));
            }
            if let Some(matched) =
                crate::jobs::known_telegram_submission(self.state, user.id, &targets, id).await?
            {
                return self
                    .reply(message, &reply::esc(&matched.status.describe()))
                    .await;
            }
        }
        let posts = telegram::from_forwarded(self.state, self.transport.as_ref(), message).await?;
        if posts.is_empty() {
            return self
                .reply(message, "That message has no media I can download.")
                .await;
        }
        // The media was staged only for this import; clean it up on every path.
        let files: Vec<_> = posts.iter().flat_map(|post| post.files.clone()).collect();
        // Charge only for a forward that actually produced downloadable media.
        let spent = match self
            .access
            .spend(self.state, role, user.id, QuotaKind::Import)
            .await
        {
            Ok(spent) => spent,
            Err(err) => {
                telegram::cleanup(&files);
                return Err(err);
            }
        };
        if !spent {
            telegram::cleanup(&files);
            return self.reply(message, "Daily import limit reached.").await;
        }
        let result = crate::jobs::import_posts(self.state, posts, Some(user.id), None).await;
        telegram::cleanup(&files);
        let outcome = result?;
        let summary = reply::esc(&outcome.summary());
        let text = if outcome.suggestions.is_empty() {
            format!("Archived {summary}.")
        } else {
            summary
        };
        self.reply(message, &text).await
    }

    // ---------------------------------------------------------------- browsing

    async fn status(&self, message: &BotMessage) -> Result<()> {
        let stats = self
            .state
            .db
            .call(|conn| {
                let posts: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;
                let approved: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM posts WHERE state = 'approved'",
                    [],
                    |r| r.get(0),
                )?;
                let queued: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM queue WHERE state = 'queued'",
                    [],
                    |r| r.get(0),
                )?;
                let unknown: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM queue WHERE state = 'unknown'",
                    [],
                    |r| r.get(0),
                )?;
                let sent: i64 =
                    conn.query_row("SELECT COUNT(*) FROM sends WHERE ok = 1", [], |r| r.get(0))?;
                Ok((posts, approved, queued, unknown, sent))
            })
            .await?;
        let channels = self.state.db.call(|conn| channel_db::list(conn)).await?;
        let enabled = channels.iter().filter(|c| c.enabled && !c.paused).count();
        let discovered = self
            .state
            .db
            .call(|conn| channel_db::list_discovered(conn))
            .await?
            .iter()
            .filter(|c| !c.enabled)
            .count();

        let body = format!(
            "<b>nujijushi</b> is running\n\
             posts {} ({} approved)\n\
             queued {} · sent {} · unconfirmed {}\n\
             channels {} ({} active, {} waiting for /enable)",
            stats.0,
            stats.1,
            stats.2,
            stats.4,
            stats.3,
            channels.len(),
            enabled,
            discovered
        );
        self.reply(message, &body).await
    }

    async fn list_posts(
        &self,
        message: &BotMessage,
        role: Role,
        limit: Option<&str>,
    ) -> Result<()> {
        let limit = limit
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(8)
            .clamp(1, 30);
        let show_nsfw = self.access.allows_nsfw_browse(role);
        let filter = post_db::PostFilter {
            limit,
            nsfw: if show_nsfw { None } else { Some(false) },
            ..Default::default()
        };
        let cards = self
            .state
            .db
            .call(move |conn| post_db::cards(conn, &filter))
            .await?;
        if cards.is_empty() {
            return self.reply(message, "Nothing archived yet.").await;
        }
        let mut body = String::from("<b>latest posts</b>\n");
        for card in &cards {
            body.push_str(&format!(
                "<code>#{}</code> {} · {} · {}\n",
                card.id,
                card.state.as_str(),
                reply::esc(card.author_name.as_deref().unwrap_or("unknown")),
                reply::esc(
                    &card
                        .snippet
                        .clone()
                        .unwrap_or_else(|| format!("{} media", card.media_count))
                )
            ));
        }
        if !show_nsfw {
            body.push_str("\n<sub>nsfw posts are hidden here</sub>");
        }
        self.reply(message, &body).await
    }

    async fn show_post(&self, message: &BotMessage, id: Option<&str>) -> Result<()> {
        let Some(id) = id.and_then(|raw| raw.trim_start_matches('#').parse::<i64>().ok()) else {
            return self
                .reply(message, "Usage: <code>/show &lt;post id&gt;</code>")
                .await;
        };
        let post = match self.state.db.call(move |conn| post_db::get(conn, id)).await {
            Ok(post) => post,
            Err(_) => return self.reply(message, "No such post.").await,
        };
        let post_id = post.id;
        let (queues, deliveries) = self
            .state
            .db
            .call(move |conn| {
                Ok((
                    queue_db::for_post(conn, post_id)?,
                    queue_db::delivery_links(conn, post_id)?,
                ))
            })
            .await?;
        let channels = self.state.db.call(|conn| channel_db::list(conn)).await?;
        let mut body = format!(
            "<b>#{} {}</b>\n{} · {} · {} item(s)\n",
            post.id,
            reply::esc(&post.source),
            reply::esc(&post.author_label()),
            post.rating,
            post.media.len()
        );
        if let Some(description) = &post.description {
            body.push_str(&format!("\n{}", reply::esc(description)));
        }
        if !post.tags.is_empty() {
            body.push_str(&format!(
                "\n\n<code>{}</code>",
                reply::esc(&post.tags.join(" "))
            ));
        }
        if let Some(url) = &post.url {
            body.push_str(&format!("\n{}", reply::esc(url)));
        }
        if !queues.is_empty() {
            body.push_str("\n\n<b>delivery</b>");
            for (queue_id, channel_id, state) in &queues {
                let channel = channels
                    .iter()
                    .find(|channel| channel.id == *channel_id)
                    .map(|channel| channel.alias.clone())
                    .unwrap_or_else(|| format!("#{channel_id}"));
                let url = deliveries
                    .iter()
                    .find(|delivery| delivery.channel_id == *channel_id)
                    .map(|delivery| format!(" · {}", reply::esc(&delivery.url)))
                    .unwrap_or_default();
                body.push_str(&format!(
                    "\n<code>q#{queue_id}</code> {} · {}{url}",
                    reply::esc(&channel),
                    state.as_str()
                ));
            }
        }
        self.reply(message, &body).await
    }

    async fn channels(&self, message: &BotMessage) -> Result<()> {
        let channels = self.state.db.call(|conn| channel_db::list(conn)).await?;
        if channels.is_empty() {
            return self
                .reply(message, "No channels yet. Add one on the admin side.")
                .await;
        }
        let now = chrono::Utc::now();
        let mut body = String::from("<b>channels</b>\n");
        for channel in &channels {
            let channel_id = channel.id;
            let queued = self
                .state
                .db
                .call(move |conn| queue_db::count_state(conn, channel_id, QueueState::Queued))
                .await?;
            let status = if !channel.enabled {
                "disabled".to_string()
            } else if channel.paused {
                "paused".to_string()
            } else {
                crate::queue::scheduler::slot_reason(channel, now, 0)
            };
            body.push_str(&format!(
                "<code>{}</code> — {}\n  {} queued · every {}m · {}\n",
                reply::esc(&channel.alias),
                reply::esc(&channel.title),
                queued,
                channel.interval_minutes,
                reply::esc(&status)
            ));
        }
        self.reply(message, &body).await
    }

    async fn queue(&self, message: &BotMessage, alias: Option<&str>) -> Result<()> {
        let Some(alias) = alias else {
            return self
                .reply(message, "Usage: <code>/queue &lt;alias&gt;</code>")
                .await;
        };
        let Some(channel) = self.find_channel(alias).await? else {
            return self.reply(message, "No such channel.").await;
        };
        let channel_id = channel.id;
        let entries = self
            .state
            .db
            .call(move |conn| queue_db::entries(conn, channel_id, &[QueueState::Queued], 15))
            .await?;
        if entries.is_empty() {
            return self
                .reply(
                    message,
                    &format!("<b>{}</b> has nothing queued.", reply::esc(&channel.alias)),
                )
                .await;
        }
        let mut body = format!("<b>{}</b>\n", reply::esc(&channel.alias));
        let now = chrono::Utc::now();
        let projected = crate::tg::publish::project_queue(self.state, &channel, entries.len())
            .await
            .unwrap_or_default();
        for entry in &entries {
            let eta = projected
                .iter()
                .find(|projected| projected.queue_id == entry.item.id)
                .map(|projected| {
                    format!(
                        " · est. {}",
                        crate::tg::publish::slot_label(projected.slot, now)
                    )
                })
                .unwrap_or_default();
            let attempts = if entry.item.attempts > 1 {
                format!(" · {} attempts", entry.item.attempts)
            } else {
                String::new()
            };
            let error = entry
                .item
                .last_error
                .as_deref()
                .map(|error| format!("\n  {}", reply::esc(error)))
                .unwrap_or_default();
            body.push_str(&format!(
                "<code>q#{}</code> post #{} · {}{eta}{attempts}{error}\n",
                entry.item.id,
                entry.item.post_id,
                if entry.item.pinned {
                    "pinned"
                } else {
                    "queued"
                }
            ));
        }
        body.push_str("\n/queue shows the first 15.");
        self.reply(message, &body).await
    }

    // ---------------------------------------------------------------- queueing

    async fn push(
        &self,
        message: &BotMessage,
        user: &BotUser,
        role: Role,
        alias: Option<&str>,
        post_id: Option<&str>,
    ) -> Result<()> {
        let Some(alias) = alias else {
            return self
                .reply(
                    message,
                    "Usage: <code>/push &lt;alias&gt; [post id]</code>\nQueues the post; publishing is done by the schedule.",
                )
                .await;
        };
        let Some(channel) = self.find_channel(alias).await? else {
            return self.reply(message, "No such channel.").await;
        };

        let approved_only = self.state.cfg.guest_reputation && !role.is_admin();
        let post_id = match post_id {
            Some(raw) => raw.trim_start_matches('#').parse::<i64>().ok(),
            None => {
                let channel_id = channel.id;
                self.state
                    .db
                    .call(move |conn| post_db::next_unenqueued(conn, channel_id, approved_only))
                    .await?
            }
        };
        let Some(post_id) = post_id else {
            return self
                .reply(message, "Nothing eligible left to queue for this channel.")
                .await;
        };

        // Only spend once there is a concrete post to queue.
        if !self
            .access
            .spend(self.state, role, user.id, QuotaKind::Queue)
            .await?
        {
            return self.reply(message, "Daily queue limit reached.").await;
        }
        let outcome =
            match crate::queue::enqueue_as(self.state, &channel, post_id, 0, role.is_admin()).await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    let _ = self
                        .access
                        .refund(self.state, user.id, QuotaKind::Queue)
                        .await;
                    return Err(err);
                }
            };
        if !matches!(&outcome, crate::queue::EnqueueOutcome::Queued(_)) {
            // Already queued or vetoed: the action did not add anything.
            let _ = self
                .access
                .refund(self.state, user.id, QuotaKind::Queue)
                .await;
        }
        self.reply(
            message,
            &format!(
                "post #{} → <code>{}</code>: {}",
                post_id,
                reply::esc(&channel.alias),
                reply::esc(&outcome.describe())
            ),
        )
        .await
    }

    async fn publish(&self, message: &BotMessage, alias: Option<&str>) -> Result<()> {
        let Some(alias) = alias else {
            return self
                .reply(message, "Usage: <code>/publish &lt;alias&gt;</code>")
                .await;
        };
        let Some(channel) = self.find_channel(alias).await? else {
            return self.reply(message, "No such channel.").await;
        };
        let outcome =
            crate::tg::publish::publish_channel(self.state, &channel, false, true).await?;
        self.reply(message, &reply::esc(&outcome.describe())).await
    }

    // ---------------------------------------------------------------- curation

    async fn set_state(
        &self,
        message: &BotMessage,
        role: Role,
        args: &[String],
        state: PostState,
    ) -> Result<()> {
        let approval = state == PostState::Approved;
        let usage = if approval && role.is_admin() && self.state.cfg.guest_reputation {
            "Usage: <code>/approve &lt;id…&gt;</code> or <code>/approve --points &lt;5–20&gt; &lt;id&gt;</code>"
        } else if approval {
            "Usage: <code>/approve &lt;id…&gt;</code>"
        } else {
            "Usage: <code>/reject &lt;id…&gt;</code>"
        };
        let (ids, points) = match args {
            [flag, amount, id] if flag == "--points" && approval && role.is_admin() => {
                let Ok(points) = amount.parse::<i64>() else {
                    return self.reply(message, usage).await;
                };
                (parse_ids(std::slice::from_ref(id)), Some(points))
            }
            _ if args.iter().any(|arg| arg.starts_with("--")) => {
                return self.reply(message, usage).await;
            }
            _ => (parse_ids(args), None),
        };
        if ids.is_empty() || (points.is_some() && ids.len() != 1) {
            return self.reply(message, usage).await;
        }
        let count = ids.len();
        let enabled = self.state.cfg.guest_reputation;
        let result = self
            .state
            .db
            .call(move |conn| {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                for id in ids {
                    crate::db::reputation::review(
                        &tx,
                        id,
                        state,
                        enabled,
                        role.is_admin(),
                        points,
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await;
        match result {
            Ok(()) => {
                self.reply(message, &format!("{count} post(s) → {state}"))
                    .await
            }
            Err(crate::error::Error::Invalid(reason)) => {
                self.reply(message, &reply::esc(&reason)).await
            }
            Err(err) => Err(err),
        }
    }

    async fn set_nsfw(
        &self,
        message: &BotMessage,
        id: Option<&str>,
        value: Option<&str>,
    ) -> Result<()> {
        let (Some(id), Some(value)) = (
            id.and_then(|raw| raw.trim_start_matches('#').parse::<i64>().ok()),
            value,
        ) else {
            return self
                .reply(message, "Usage: <code>/nsfw &lt;id&gt; auto|yes|no</code>")
                .await;
        };
        let flag = match value {
            "auto" => None,
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => {
                return self
                    .reply(
                        message,
                        "Expected <code>auto</code>, <code>yes</code> or <code>no</code>.",
                    )
                    .await;
            }
        };
        self.state
            .db
            .call(move |conn| post_db::set_nsfw_override(conn, id, flag))
            .await?;
        self.reply(
            message,
            &format!(
                "post #{id}: nsfw = {}",
                if flag == Some(true) {
                    "yes"
                } else if flag == Some(false) {
                    "no"
                } else {
                    "auto"
                }
            ),
        )
        .await
    }

    async fn set_rating(
        &self,
        message: &BotMessage,
        id: Option<&str>,
        rating: Option<&str>,
    ) -> Result<()> {
        let (Some(id), Some(rating)) = (
            id.and_then(|raw| raw.trim_start_matches('#').parse::<i64>().ok()),
            rating,
        ) else {
            return self
                .reply(
                    message,
                    "Usage: <code>/rate &lt;id&gt; sfw|questionable|explicit</code>",
                )
                .await;
        };
        let Ok(rating) = rating.parse::<Rating>() else {
            return self.reply(message, "Unknown rating.").await;
        };
        self.state
            .db
            .call(move |conn| post_db::set_rating(conn, id, rating))
            .await?;
        self.reply(message, &format!("post #{id}: {rating}")).await
    }

    async fn set_tags(
        &self,
        message: &BotMessage,
        id: Option<&str>,
        tags: &[String],
    ) -> Result<()> {
        let Some(id) = id.and_then(|raw| raw.trim_start_matches('#').parse::<i64>().ok()) else {
            return self
                .reply(message, "Usage: <code>/tag &lt;id&gt; tag1 tag2 …</code>")
                .await;
        };
        if tags.is_empty() {
            return self.reply(message, "No tags given.").await;
        }
        let tags: Vec<String> = tags.to_vec();
        let count = tags.len();
        self.state
            .db
            .call(move |conn| {
                crate::db::transaction(conn, |conn| crate::db::tags::add_manual(conn, id, &tags))
            })
            .await?;
        self.reply(message, &format!("post #{id}: added {count} tag(s)"))
            .await
    }

    // ---------------------------------------------------------------- admin

    async fn toggle_pause(
        &self,
        message: &BotMessage,
        alias: Option<&str>,
        paused: bool,
    ) -> Result<()> {
        let Some(channel) = self.find_channel_arg(message, alias).await? else {
            return Ok(());
        };
        let id = channel.id;
        self.state
            .db
            .call(move |conn| channel_db::set_paused(conn, id, paused))
            .await?;
        self.reply(
            message,
            &format!(
                "<code>{}</code> {}",
                reply::esc(&channel.alias),
                if paused { "paused" } else { "resumed" }
            ),
        )
        .await
    }

    async fn toggle_enabled(
        &self,
        message: &BotMessage,
        alias: Option<&str>,
        enabled: bool,
    ) -> Result<()> {
        let Some(channel) = self.find_channel_arg(message, alias).await? else {
            return Ok(());
        };
        let id = channel.id;
        self.state
            .db
            .call(move |conn| {
                channel_db::set_enabled(conn, id, enabled)?;
                if enabled {
                    channel_db::set_paused(conn, id, false)?;
                }
                Ok(())
            })
            .await?;
        self.reply(
            message,
            &format!(
                "<code>{}</code> {}",
                reply::esc(&channel.alias),
                if enabled {
                    "enabled — it will publish on its next slot"
                } else {
                    "disabled"
                }
            ),
        )
        .await
    }

    async fn discover(&self, message: &BotMessage) -> Result<()> {
        let channels = self
            .state
            .db
            .call(|conn| channel_db::list_discovered(conn))
            .await?;
        if channels.is_empty() {
            return self
                .reply(
                    message,
                    "No channels discovered yet.\nAdd the bot as an administrator of a channel and it will appear here.",
                )
                .await;
        }
        let mut body = String::from("<b>discovered channels</b>\n");
        for channel in &channels {
            body.push_str(&format!(
                "<code>{}</code> — {} · {}\n",
                reply::esc(&channel.alias),
                reply::esc(&channel.title),
                if channel.enabled {
                    "enabled"
                } else {
                    "/enable to start"
                }
            ));
        }
        self.reply(message, &body).await
    }

    async fn users(&self, message: &BotMessage) -> Result<()> {
        let users = self.state.db.call(|conn| user_db::list(conn, 25)).await?;
        if users.is_empty() {
            return self.reply(message, "No users recorded yet.").await;
        }
        let mut body = String::from("<b>users</b>\n");
        for user in &users {
            let config_admin = self.access.config().is_admin(user.tg_id);
            // the effective role: a config admin keeps a `guest` row from their
            // first message, and listing that row read like a broken config
            let role = crate::bot::access::display_role(config_admin, &user.role);
            body.push_str(&format!(
                "<code>{}</code> {} · {}{}\n",
                user.tg_id,
                role,
                reply::esc(&user.label()),
                if config_admin { " (config admin)" } else { "" }
            ));
        }
        self.reply(message, &body).await
    }

    async fn grant(
        &self,
        message: &BotMessage,
        target: Option<&str>,
        role: Option<&str>,
    ) -> Result<()> {
        let (Some(target), Some(role)) = (target, role) else {
            return self
                .reply(
                    message,
                    "Usage: <code>/grant &lt;user id&gt; admin|guest|banned</code>",
                )
                .await;
        };
        let Ok(tg_id) = target.parse::<i64>() else {
            return self.reply(message, "That is not a telegram user id.").await;
        };
        let role = match role {
            "admin" | "guest" | "banned" => role,
            _ => {
                return self
                    .reply(message, "Role must be admin, guest or banned.")
                    .await;
            }
        };
        if role != "admin" && self.access.config().is_admin(tg_id) {
            return self
                .reply(
                    message,
                    "That id is an admin in config.toml — edit the config to demote them.",
                )
                .await;
        }
        let role_owned = role.to_string();
        self.state
            .db
            .call(move |conn| user_db::set_role(conn, tg_id, &role_owned))
            .await?;
        super::menus::sync_stored_user(self.state, tg_id).await;
        self.reply(message, &format!("<code>{tg_id}</code> → {role}"))
            .await
    }

    // ---------------------------------------------------------------- helpers

    async fn find_channel(&self, needle: &str) -> Result<Option<Channel>> {
        let needle = needle.trim().trim_start_matches('@').to_string();
        self.state
            .db
            .call(move |conn| channel_db::find(conn, &needle))
            .await
    }

    async fn find_channel_arg(
        &self,
        message: &BotMessage,
        alias: Option<&str>,
    ) -> Result<Option<Channel>> {
        let Some(alias) = alias else {
            self.reply(message, "Which channel? Use the alias from /channels.")
                .await?;
            return Ok(None);
        };
        match self.find_channel(alias).await? {
            Some(channel) => Ok(Some(channel)),
            None => {
                self.reply(message, "No such channel.").await?;
                Ok(None)
            }
        }
    }
}

/// `1 2 #3` -> [1, 2, 3]
pub fn parse_ids(args: &[String]) -> Vec<i64> {
    args.iter()
        .flat_map(|arg| arg.split([',', ' ']))
        .filter_map(|raw| raw.trim().trim_start_matches('#').parse::<i64>().ok())
        .collect()
}

fn not_allowed(user: &BotUser) -> String {
    format!(
        "This bot is private.\nYour id is <code>{}</code> — ask the owner to add it, or send /whoami.",
        user.id
    )
}

fn whoami_text(user: &BotUser, role: Role) -> String {
    format!(
        "<b>you</b>\nid <code>{}</code>\n{}\nrole {}",
        user.id,
        reply::esc(&user.label()),
        role.as_str()
    )
}

fn start_text(access: &Access, user: &BotUser, role: Role) -> String {
    let import_intro = if access.allows(role, Perm::Import) {
        "Send me a link or forward a post from a channel to archive it.\n"
    } else {
        ""
    };
    format!(
        "<b>nujijushi</b>\n{import_intro}Your id: <code>{}</code> · role {}\n\n{}",
        user.id,
        role.as_str(),
        help_text(access, role)
    )
}

fn help_text(access: &Access, role: Role) -> String {
    use std::fmt::Write as _;

    let mut text = String::from("<b>commands</b>");
    for command in commands::available(access, role) {
        let _ = write!(
            text,
            "\n<code>/{}{}</code> — {}",
            command.name,
            reply::esc(command.args),
            reply::esc(command.description)
        );
    }
    text
}

/// Tell the admins that a guest submitted links.
///
/// The importer records `submitted_by`, so the web grid can badge it, but a
/// Telegram DM is what actually gets noticed. Failures are logged, never fatal:
/// the import must not hinge on a notification.
async fn notify_admins_of_submission(
    state: &crate::state::AppState,
    user: &BotUser,
    targets: &[String],
) {
    let Some(transport) = state.transport.as_deref() else {
        return;
    };
    let admins = match crate::bot::admin_recipients(state, Some(user.id)).await {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!("cannot list the admins to notify: {err}");
            return;
        }
    };
    if admins.is_empty() {
        return;
    }
    let who = match (&user.username, &user.first_name) {
        (Some(username), _) => format!("@{username}"),
        (None, Some(name)) => name.clone(),
        _ => user.id.to_string(),
    };
    let links = targets
        .iter()
        .map(|target| format!("• <code>{}</code>", reply::esc(target)))
        .collect::<Vec<_>>()
        .join("\n");
    let html = format!(
        "📥 <b>{}</b> (<code>{}</code>) is not an admin and submitted {} link(s):\n{}",
        reply::esc(&who),
        user.id,
        targets.len(),
        links
    );
    for admin in admins {
        if let Err(err) = reply::send(transport, &admin.to_string(), &html).await {
            tracing::warn!(admin, "cannot notify an admin about a submission: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_id_lists() {
        let args: Vec<String> = ["1", "#2,", "3"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_ids(&args), vec![1, 2, 3]);
        assert!(parse_ids(&[]).is_empty());
    }

    #[test]
    fn denial_mentions_the_user_id() {
        let user = BotUser {
            id: 4242,
            username: None,
            first_name: None,
            is_bot: false,
        };
        assert!(not_allowed(&user).contains("4242"));
    }
}
