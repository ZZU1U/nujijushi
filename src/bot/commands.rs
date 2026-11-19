//! Command parsing and the help text shown in Telegram.

use crate::bot::access::{Access, Perm, Role};

#[derive(Clone, Copy)]
pub(super) enum Requirement {
    AllowedUser,
    Permission(Perm),
    Admin,
}

pub(super) struct CommandSpec {
    pub name: &'static str,
    pub args: &'static str,
    pub description: &'static str,
    pub requirement: Requirement,
}

impl CommandSpec {
    const fn new(
        name: &'static str,
        args: &'static str,
        description: &'static str,
        requirement: Requirement,
    ) -> Self {
        Self {
            name,
            args,
            description,
            requirement,
        }
    }

    pub(super) fn available_to(&self, access: &Access, role: Role) -> bool {
        match self.requirement {
            Requirement::AllowedUser => role != Role::Banned,
            Requirement::Permission(perm) => access.allows(role, perm),
            Requirement::Admin => role.is_admin(),
        }
    }
}

use self::Requirement::{Admin, AllowedUser, Permission};

pub(super) const MENU: &[CommandSpec] = &[
    CommandSpec::new("start", "", "introduce the bot", AllowedUser),
    CommandSpec::new("help", "", "show commands you can use", AllowedUser),
    CommandSpec::new(
        "import",
        " <link>…",
        "archive links (or just send them)",
        Permission(Perm::Import),
    ),
    CommandSpec::new("ls", " [n]", "list recent posts", Permission(Perm::Browse)),
    CommandSpec::new("show", " <id>", "show one post", Permission(Perm::Browse)),
    CommandSpec::new(
        "push",
        " <alias> [id]",
        "queue a post",
        Permission(Perm::Queue),
    ),
    CommandSpec::new(
        "queue",
        " <alias>",
        "show what is lined up",
        Permission(Perm::Browse),
    ),
    CommandSpec::new(
        "channels",
        "",
        "list channel aliases and queue depth",
        Permission(Perm::Browse),
    ),
    CommandSpec::new(
        "publish",
        " <alias>",
        "publish one post now",
        Permission(Perm::Publish),
    ),
    CommandSpec::new(
        "approve",
        " <id>…",
        "approve posts",
        Permission(Perm::Curate),
    ),
    CommandSpec::new("reject", " <id>…", "reject posts", Permission(Perm::Curate)),
    CommandSpec::new(
        "nsfw",
        " <id> <auto|yes|no>",
        "override the NSFW flag",
        Permission(Perm::Curate),
    ),
    CommandSpec::new(
        "rate",
        " <id> <sfw|questionable|explicit>",
        "set the rating",
        Permission(Perm::Curate),
    ),
    CommandSpec::new(
        "tag",
        " <id> <tags>",
        "add manual tags",
        Permission(Perm::Curate),
    ),
    CommandSpec::new("pause", " <alias>", "pause a channel", Admin),
    CommandSpec::new("resume", " <alias>", "resume a channel", Admin),
    CommandSpec::new("enable", " <alias>", "enable a channel", Admin),
    CommandSpec::new("disable", " <alias>", "disable a channel", Admin),
    CommandSpec::new("discover", "", "list channels the bot was added to", Admin),
    CommandSpec::new("users", "", "list known users", Admin),
    CommandSpec::new(
        "grant",
        " <id> <admin|guest|banned>",
        "set a user role",
        Admin,
    ),
    CommandSpec::new("ban", " <id>", "ban a user", Admin),
    CommandSpec::new("whoami", "", "show your Telegram ID", AllowedUser),
    CommandSpec::new("status", "", "show archive and queue health", AllowedUser),
];

pub(super) fn available(access: &Access, role: Role) -> impl Iterator<Item = &'static CommandSpec> {
    MENU.iter()
        .filter(move |command| command.available_to(access, role))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub args: Vec<String>,
    pub raw_args: String,
}

impl Command {
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(|arg| arg.as_str())
    }

    pub fn rest(&self) -> &str {
        self.raw_args.trim()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// `/name args`
    Command(Command),
    /// Plain text that is not a command (a link, a question, ...).
    Text(String),
    Empty,
}

/// Parse an incoming message body.
///
/// Handles `/cmd`, `/cmd@this_bot`, `/cmd arg`, and treats anything else as
/// plain text (so a bare link can be imported without a command).
pub fn parse(text: &str, bot_username: Option<&str>) -> Parsed {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Parsed::Empty;
    }
    let Some(rest) = trimmed.strip_prefix('/') else {
        return Parsed::Text(trimmed.to_string());
    };
    // a command name is the first word; `/start` alone is fine
    let (head, tail) = match rest.split_once(char::is_whitespace) {
        Some((head, tail)) => (head, tail),
        None => (rest, ""),
    };
    let (name, target) = match head.split_once('@') {
        Some((name, target)) => (name, Some(target)),
        None => (head, None),
    };
    if let (Some(target), Some(own)) = (target, bot_username)
        && !target.eq_ignore_ascii_case(own.trim_start_matches('@'))
    {
        // addressed to another bot in the same group
        return Parsed::Text(trimmed.to_string());
    }
    let name = name.to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Parsed::Text(trimmed.to_string());
    }
    Parsed::Command(Command {
        name,
        args: tail.split_whitespace().map(|arg| arg.to_string()).collect(),
        raw_args: tail.to_string(),
    })
}

/// Every link in a message, normalised and de-duplicated in the order sent.
///
/// People paste batches (one link per line, or several on one line, sometimes
/// with a word around them), so collect the link-looking tokens. Keeping only
/// the first one silently swallowed the rest of the batch. What counts as a
/// link belongs to the source layer, not to this interface.
pub fn import_targets(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in crate::source::split_pasted_links(text)
        .into_iter()
        .flat_map(str::split_whitespace)
    {
        let target = crate::source::normalize_target(token);
        if !crate::source::looks_importable(&target) {
            continue;
        }
        if !out.contains(&target) {
            out.push(target);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_commands() {
        match parse("/ls 5", Some("@nujijushi")) {
            Parsed::Command(cmd) => {
                assert_eq!(cmd.name, "ls");
                assert_eq!(cmd.arg(0), Some("5"));
            }
            other => panic!("expected a command, got {other:?}"),
        }
        match parse("/start", None) {
            Parsed::Command(cmd) => assert!(cmd.args.is_empty()),
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn ignores_commands_for_other_bots() {
        assert!(matches!(
            parse("/ls@other_bot 3", Some("@nujijushi")),
            Parsed::Text(_)
        ));
        assert!(matches!(
            parse("/ls@nujijushi 3", Some("@nujijushi")),
            Parsed::Command(_)
        ));
        // no username context: accept it
        assert!(matches!(parse("/ls@nujijushi", None), Parsed::Command(_)));
    }

    #[test]
    fn keeps_raw_args_for_tags() {
        let Parsed::Command(cmd) = parse("/tag 12 blue sky, clouds", None) else {
            panic!("expected command");
        };
        assert_eq!(cmd.arg(0), Some("12"));
        assert_eq!(cmd.rest(), "12 blue sky, clouds");
    }

    #[test]
    fn rejects_non_commands() {
        assert!(matches!(parse("hello there", None), Parsed::Text(_)));
        assert!(matches!(parse("   ", None), Parsed::Empty));
        assert!(matches!(parse("/", None), Parsed::Text(_)));
        assert!(matches!(parse("/!!!", None), Parsed::Text(_)));
    }

    #[test]
    fn collects_every_link_in_a_message() {
        assert_eq!(
            import_targets("https://www.pixiv.net/artworks/1"),
            vec!["https://www.pixiv.net/artworks/1"]
        );
        // t.me links get a scheme, and duplicates collapse
        assert_eq!(
            import_targets("t.me/night_art/42\nt.me/night_art/42"),
            vec!["https://t.me/night_art/42"]
        );
        // a whole batch, one per line, keeps its order
        assert_eq!(
            import_targets(
                "https://x.com/a/status/1\nhttps://x.com/a/status/2\nhttps://x.com/a/status/3"
            ),
            vec![
                "https://x.com/a/status/1",
                "https://x.com/a/status/2",
                "https://x.com/a/status/3"
            ]
        );
        // several on one line, glued to punctuation, with a word around them
        assert_eq!(
            import_targets("look: <https://x.com/a/status/1>, https://x.com/a/status/2."),
            vec!["https://x.com/a/status/1", "https://x.com/a/status/2"]
        );
        assert!(import_targets("just chatting").is_empty());
        assert!(import_targets("").is_empty());
    }

    #[test]
    fn menu_entries_are_telegram_safe() {
        for CommandSpec {
            name: command,
            description,
            ..
        } in MENU
        {
            assert!(
                command.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{command} is not a legal command name"
            );
            assert!(!description.is_empty());
            assert!(description.chars().count() <= 256);
        }
    }
}
