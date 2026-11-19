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
