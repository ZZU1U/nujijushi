//! Form payloads posted by the admin UI.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    pub csrf: String,
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
    /// `yes` = only posts already out, `all` = include them; absent = hide them.
    #[serde(default)]
    pub sent: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub setup: bool,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub page: Option<i64>,
    #[serde(default)]
    pub per_page: Option<String>,
    pub cursor: Option<String>,
    #[serde(default)]
    pub flash: Option<String>,
    pub next: Option<String>,
}

pub const POST_PAGE_SIZES: [i64; 3] = [24, 48, 96];

pub fn post_page_size(value: &str) -> Option<i64> {
    value
        .parse()
        .ok()
        .filter(|size| POST_PAGE_SIZES.contains(size))
}

impl PostFilter {
    pub fn tag_url(&self, tag: Option<&str>) -> String {
        let mut query = self.filter_query(tag);
        let query = query.finish();
        if query.is_empty() {
            "/posts".into()
        } else {
            format!("/posts?{query}")
        }
    }

    fn filter_query(&self, tag: Option<&str>) -> url::form_urlencoded::Serializer<'static, String> {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in [
            ("search", &self.search),
            ("state", &self.state),
            ("source", &self.source),
            ("nsfw", &self.nsfw),
            ("sent", &self.sent),
        ] {
            if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
                query.append_pair(key, value);
            }
        }
        if self.setup {
            query.append_pair("setup", "true");
        }
        if let Some(tag) = tag.filter(|tag| !tag.is_empty()) {
            query.append_pair("tag", tag);
        }
        query
    }

    pub fn page_url(&self, page: i64, per_page: i64) -> String {
        let mut query = self.filter_query(self.tag.as_deref());
        query.append_pair("per_page", &per_page.to_string());
        query.append_pair("page", &page.to_string());
        format!("/posts?{}", query.finish())
    }
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

/// Deserialize an optional number from a field the browser may submit empty.
///
/// `<select><option value="">` and a cleared `<input type=number>` are sent as
/// `name=`, and serde's plain `Option<i64>` rejects the empty string — which
/// failed the *whole* form body ("cannot parse integer from empty string") and
/// turned e.g. "import these links, don't queue them" into an error page.
pub fn empty_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(text) if text.trim().is_empty() => Ok(None),
        Some(text) => text
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Deserialize)]
pub struct PostDetailQuery {
    pub flash: Option<String>,
    pub section: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PostDelete {
    #[serde(default)]
    pub csrf: String,
    pub imported_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct PostEdit {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
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
    #[serde(default, deserialize_with = "empty_as_none")]
    pub approval_points: Option<i64>,
    #[serde(default)]
    pub csrf: String,
}

#[derive(Debug, Deserialize)]
pub struct PostTagAction {
    #[serde(default)]
    pub csrf: String,
    pub tag_id: Option<i64>,
    pub tags: Option<String>,
    pub action: String,
}

#[derive(Debug, Deserialize)]
pub struct ChannelForm {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub alias: Option<String>,
    pub title: String,
    pub tg_id: String,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub interval_minutes: Option<i64>,
    #[serde(default)]
    pub active_from: Option<String>,
    #[serde(default)]
    pub active_to: Option<String>,
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub daily_cap: Option<i64>,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub jitter_secs: Option<i64>,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub artist_cooldown: Option<i64>,
    #[serde(default)]
    pub nsfw_policy: Option<String>,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub min_media: Option<i64>,
    #[serde(default)]
    pub blocked_tags: Option<String>,
    #[serde(default)]
    pub required_tags: Option<String>,
    #[serde(default)]
    pub tag_style: Option<String>,
    #[serde(default, deserialize_with = "empty_as_none")]
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
    /// One target per line (URL or path), also accepts pasted encoded line breaks.
    pub targets: String,
    #[serde(default)]
    pub reuse_active: bool,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub channel: Option<i64>,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub priority: Option<i64>,
}

impl ImportForm {
    pub fn target_list(&self) -> Vec<String> {
        self.targets
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .flat_map(crate::source::split_pasted_links)
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
    #[serde(default, deserialize_with = "empty_as_none")]
    pub channel: Option<i64>,
    #[serde(default, deserialize_with = "empty_as_none")]
    pub message_id: Option<i32>,
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
            targets: "https://x/1\n\n # comment\nhttps://x/2%0Ahttps://x/3  \n".into(),
            reuse_active: false,
            channel: None,
            priority: None,
        };
        assert_eq!(
            form.target_list(),
            vec!["https://x/1", "https://x/2", "https://x/3"]
        );
    }

    #[test]
    fn checkbox_flags() {
        assert!(ChannelForm::flag(&Some("on".into())));
        assert!(!ChannelForm::flag(&None));
        assert!(!ChannelForm::flag(&Some("false".into())));
    }

    fn import_form(body: serde_json::Value) -> Result<ImportForm, serde_json::Error> {
        serde_json::from_value(body)
    }

    #[test]
    fn an_empty_select_or_number_input_is_none() {
        // the exact shape the import page posts when "don't queue" is chosen
        let form = import_form(serde_json::json!({
            "csrf": "tok",
            "targets": "https://x.com/a/status/1\nhttps://x.com/b/status/2",
            "channel": "",
            "priority": "  ",
        }))
        .unwrap();
        assert_eq!(form.channel, None);
        assert_eq!(form.priority, None);
        assert_eq!(form.target_list().len(), 2);
    }

    #[test]
    fn a_filled_number_is_parsed_and_junk_is_an_error() {
        let form = import_form(serde_json::json!({
            "csrf": "tok",
            "targets": "https://x.com/a/status/1",
            "channel": "3",
        }))
        .unwrap();
        assert_eq!(form.channel, Some(3));

        let bad = import_form(serde_json::json!({
            "csrf": "tok",
            "targets": "https://x.com/a/status/1",
            "channel": "abc",
        }));
        assert!(bad.is_err(), "junk must still be rejected");
    }

    #[test]
    fn cleared_channel_numbers_are_none_too() {
        // every numeric field of the channel form tolerates an empty submission
        let form: ChannelForm = serde_json::from_value(serde_json::json!({
            "title": "Night",
            "tg_id": "@night",
            "interval_minutes": "",
            "daily_cap": "",
            "jitter_secs": "",
            "artist_cooldown": "",
            "min_media": "",
            "max_tags": "12",
        }))
        .unwrap();
        assert_eq!(form.interval_minutes, None);
        assert_eq!(form.daily_cap, None);
        assert_eq!(form.jitter_secs, None);
        assert_eq!(form.artist_cooldown, None);
        assert_eq!(form.min_media, None);
        assert_eq!(form.max_tags, Some(12));
    }
}
