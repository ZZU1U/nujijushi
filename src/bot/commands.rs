//! Command parsing and the help text shown in Telegram.

/// Command menu registered with `setMyCommands`.
pub const MENU: &[(&str, &str)] = &[
    ("start", "what this bot does"),
    ("help", "commands you can use"),
    ("import", "import a link (or just send it)"),
    ("ls", "recent posts"),
    ("show", "one post by id"),
    ("push", "queue a post into a channel"),
    ("queue", "what is lined up"),
    ("channels", "channels and their aliases"),
    ("publish", "publish one now (admin)"),
    ("approve", "approve posts"),
    ("reject", "reject posts"),
    ("nsfw", "override the nsfw flag"),
    ("rate", "set the rating"),
    ("tag", "add manual tags"),
    ("pause", "stop publishing to a channel"),
    ("resume", "resume a channel"),
    ("enable", "start publishing to a discovered channel"),
    ("disable", "stop publishing to a channel"),
    ("discover", "channels the bot was added to"),
    ("users", "known users"),
    ("grant", "set a user role"),
    ("ban", "deny a user"),
    ("whoami", "your telegram id"),
    ("status", "archive and queue health"),
];

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

/// Does this text look like something we should treat as an import target?
pub fn looks_like_link(text: &str) -> bool {
    let lowered = text.trim().to_ascii_lowercase();
    lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("t.me/")
}

/// The whole body is only a link or two (so a message with commentary is not an import).
pub fn import_target(text: &str) -> Option<String> {
    let mut parts = text.split_whitespace();
    let first = parts.next()?;
    if !looks_like_link(first) {
        return None;
    }
    let rest_ok = parts.all(looks_like_link);
    if !rest_ok {
        return None;
    }
    Some(normalize_target(first))
}

/// `t.me/x/1` -> `https://t.me/x/1`
pub fn normalize_target(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.to_ascii_lowercase().starts_with("t.me/") {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    }
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
    fn detects_importable_links() {
        assert_eq!(
            import_target("https://www.pixiv.net/artworks/1").as_deref(),
            Some("https://www.pixiv.net/artworks/1")
        );
        assert_eq!(
            import_target("t.me/night_art/42").as_deref(),
            Some("https://t.me/night_art/42")
        );
        assert_eq!(
            import_target("https://x.com/a/status/1 https://x.com/a/status/2").as_deref(),
            Some("https://x.com/a/status/1")
        );
        assert_eq!(import_target("look at this https://x.com/a/status/1"), None);
        assert_eq!(import_target("just chatting"), None);
    }

    #[test]
    fn menu_entries_are_telegram_safe() {
        for (command, description) in MENU {
            assert!(
                command.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{command} is not a legal command name"
            );
            assert!(!description.is_empty());
            assert!(description.chars().count() <= 256);
        }
    }
}
