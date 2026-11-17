//! Form payloads posted by the admin UI.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub token: String,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PostFilter {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub nsfw: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub flash: Option<String>,
}

/// Raw form map: checkbox lists arrive as repeated keys (`sel_<id>`).
pub type FormMap = std::collections::HashMap<String, String>;

pub fn field<'a>(form: &'a FormMap, key: &str) -> Option<&'a str> {
    form.get(key).map(|value| value.as_str())
}

/// IDs selected in a list form (`sel_12=on`).
pub fn selected_ids(form: &FormMap) -> Vec<i64> {
    let mut ids: Vec<i64> = form
        .keys()
        .filter_map(|key| key.strip_prefix("sel_"))
        .filter_map(|raw| raw.parse::<i64>().ok())
        .collect();
    ids.sort_unstable();
    ids
}

#[derive(Debug, Deserialize)]
pub struct PostEdit {
    #[serde(default)]
    pub rating: Option<String>,
    /// auto | yes | no
    #[serde(default)]
    pub nsfw: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub csrf: String,
}

#[derive(Debug, Deserialize)]
pub struct ChannelForm {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub alias: Option<String>,
    pub title: String,
    pub tg_id: String,
    #[serde(default)]
    pub interval_minutes: Option<i64>,
    #[serde(default)]
    pub active_from: Option<String>,
    #[serde(default)]
    pub active_to: Option<String>,
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub daily_cap: Option<i64>,
    #[serde(default)]
    pub jitter_secs: Option<i64>,
    #[serde(default)]
    pub artist_cooldown: Option<i64>,
    #[serde(default)]
    pub nsfw_policy: Option<String>,
    #[serde(default)]
    pub min_media: Option<i64>,
    #[serde(default)]
    pub blocked_tags: Option<String>,
    #[serde(default)]
    pub required_tags: Option<String>,
    #[serde(default)]
    pub tag_style: Option<String>,
    #[serde(default)]
    pub max_tags: Option<i64>,
    #[serde(default)]
    pub attribution: Option<String>,
    #[serde(default)]
    pub template_name: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub silent: Option<String>,
    #[serde(default)]
    pub protect: Option<String>,
    #[serde(default)]
    pub include_source_tag: Option<String>,
    #[serde(default)]
    pub enabled: Option<String>,
}

impl ChannelForm {
    pub fn flag(value: &Option<String>) -> bool {
        matches!(value.as_deref(), Some("on" | "true" | "1" | "yes"))
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportForm {
    #[serde(default)]
    pub csrf: String,
    /// One target per line (URL or path)
    pub targets: String,
    #[serde(default)]
    pub channel: Option<i64>,
    #[serde(default)]
    pub priority: Option<i64>,
}

impl ImportForm {
    pub fn target_list(&self) -> Vec<String> {
        self.targets
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
pub struct QueueAction {
    pub id: i64,
    /// skip | pin | unpin | reset | remove | delivered | not_delivered
    pub action: String,
    #[serde(default)]
    pub channel: Option<i64>,
    #[serde(default)]
    pub csrf: String,
}

#[derive(Debug, Deserialize)]
pub struct SimpleAction {
    pub action: String,
    #[serde(default)]
    pub csrf: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_selected_ids() {
        let mut form = FormMap::new();
        form.insert("sel_3".into(), "on".into());
        form.insert("sel_1".into(), "on".into());
        form.insert("action".into(), "approve".into());
        form.insert("sel_bad".into(), "on".into());
        assert_eq!(selected_ids(&form), vec![1, 3]);
    }

    #[test]
    fn splits_import_targets() {
        let form = ImportForm {
            csrf: String::new(),
            targets: "https://x/1\n\n # comment\nhttps://x/2  \n".into(),
            channel: None,
            priority: None,
        };
        assert_eq!(form.target_list(), vec!["https://x/1", "https://x/2"]);
    }

    #[test]
    fn checkbox_flags() {
        assert!(ChannelForm::flag(&Some("on".into())));
        assert!(!ChannelForm::flag(&None));
        assert!(!ChannelForm::flag(&Some("false".into())));
    }
}
