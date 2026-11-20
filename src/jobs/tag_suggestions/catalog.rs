//! Optional Danbooru lookups. Only catalog identifiers cross this boundary.

use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, de::DeserializeOwned};

use crate::config::Config;
use crate::error::{Error, Result};

pub(super) struct Catalog {
    client: Client,
    base: Url,
}

#[derive(Debug, PartialEq)]
pub(super) struct Match {
    pub franchise: String,
    pub wiki_title: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct Tag {
    pub id: i64,
    pub name: String,
    category: u8,
    is_deprecated: bool,
}

#[derive(Deserialize)]
struct Alias {
    antecedent_name: String,
    consequent_name: String,
    status: String,
}

#[derive(Deserialize)]
struct Wiki {
    title: String,
    body: String,
    is_deleted: bool,
}

impl Catalog {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: crate::http::client(config)?,
            base: Url::parse("https://danbooru.donmai.us/")
                .map_err(|_| Error::config("invalid Danbooru catalog address"))?,
        })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<Option<T>> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::other("invalid catalog address"))?
            .pop_if_empty()
            .extend(path);
        let response = self
            .client
            .get(url)
            .query(query)
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .map_err(|_| Error::other("Danbooru lookup unavailable"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response.error_for_status().map_err(|err| {
            Error::other(format!(
                "Danbooru lookup returned {}",
                err.status().unwrap_or(StatusCode::BAD_GATEWAY)
            ))
        })?;
        response
            .json()
            .await
            .map(Some)
            .map_err(|_| Error::other("Invalid Danbooru lookup response"))
    }

    pub async fn identify(&self, id: Option<i64>, label: &str) -> Result<Option<Tag>> {
        if let Some(id) = id
            && let Some(tag) = self
                .get::<Tag>(&["tags", &format!("{id}.json")], &[])
                .await?
            && tag.id == id
            && tag.category == 4
            && !tag.is_deprecated
        {
            return Ok(Some(tag));
        }
        if let Some(tag) = self.exact_tag(label, 4).await? {
            return Ok(Some(tag));
        }
        match self.active_alias(label).await? {
            Some(name) => self.exact_tag(&name, 4).await,
            None => Ok(None),
        }
    }

    pub async fn lookup(&self, name: &str) -> Result<Option<Match>> {
        let Some(wiki) = self
            .get::<Wiki>(&["wiki_pages", &format!("{name}.json")], &[])
            .await?
            .filter(|wiki| wiki.title == name && !wiki.is_deleted)
        else {
            return Ok(None);
        };
        let Some(title) = opening_work(&wiki.body) else {
            return Ok(None);
        };
        let franchise = match self.exact_tag(&title, 3).await? {
            Some(work) => Some(work),
            None => match self.active_alias(&title).await? {
                Some(name) => self.exact_tag(&name, 3).await?,
                None => None,
            },
        };
        Ok(franchise.map(|work| Match {
            franchise: work.name,
            wiki_title: wiki.title,
        }))
    }

    async fn active_alias(&self, name: &str) -> Result<Option<String>> {
        let mut matches = self
            .get::<Vec<Alias>>(
                &["tag_aliases.json"],
                &[
                    ("search[antecedent_name]", name),
                    ("search[status]", "active"),
                    ("limit", "2"),
                ],
            )
            .await?
            .unwrap_or_default()
            .into_iter()
            .filter(|alias| alias.antecedent_name == name && alias.status == "active");
        let first = matches.next();
        Ok(first
            .filter(|_| matches.next().is_none())
            .map(|alias| alias.consequent_name))
    }

    async fn exact_tag(&self, name: &str, category: u8) -> Result<Option<Tag>> {
        let mut matches = self
            .get::<Vec<Tag>>(
                &["tags.json"],
                &[
                    ("search[name]", name),
                    ("search[category]", &category.to_string()),
                    ("limit", "2"),
                ],
            )
            .await?
            .unwrap_or_default()
            .into_iter()
            .filter(|tag| {
                tag.id > 0 && tag.name == name && tag.category == category && !tag.is_deprecated
            });
        let first = matches.next();
        Ok(first.filter(|_| matches.next().is_none()))
    }
}

/// Recognize explicit origin phrases in the first paragraph of the character's
/// wiki. A link elsewhere might be a crossover, a related character or a costume.
fn opening_work(body: &str) -> Option<String> {
    let paragraph = body
        .trim_start()
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chunks = paragraph.split("[[");
    let mut previous = chunks.next()?;
    let mut work = None;
    let mut previous_was_work = false;
    for chunk in chunks {
        let (link, after) = chunk.split_once("]]")?;
        let prefix = previous.trim_end().to_ascii_lowercase();
        previous = after;
        let connected = previous_was_work
            && matches!(
                prefix.trim(),
                "," | "and" | "or" | ", and" | ", or" | "/" | "&"
            );
        let explicit = [
            "character from",
            "character in",
            "character of",
            "protagonist of",
            "protagonist in",
            "antagonist of",
            "antagonist in",
            "character from the",
            "character in the",
            "protagonist of the",
            "antagonist of the",
        ]
        .iter()
        .any(|suffix| {
            prefix.strip_suffix(suffix).is_some_and(|before| {
                before
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                    && !before.split_whitespace().any(|word| word == "not")
            })
        });
        previous_was_work = connected || explicit;
        if !previous_was_work {
            continue;
        }
        let title = link
            .split('|')
            .next()?
            .trim()
            .to_lowercase()
            .replace(' ', "_");
        if title.is_empty() || work.as_ref().is_some_and(|existing| *existing != title) {
            return None;
        }
        work = Some(title);
    }
    work
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, http::Uri, response::IntoResponse};
    use serde_json::json;

    use super::*;
    use crate::db::{Db, tag_suggestions};

    #[tokio::test]
    async fn resolves_an_exact_renamed_character_and_work_and_can_reuse_them_offline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let router = Router::new().fallback(|uri: Uri| async move {
            let query: std::collections::HashMap<_, _> = url::form_urlencoded::parse(
                uri.query().unwrap_or_default().as_bytes(),
            ).collect();
            let tag = |id, name, category| json!({
                "id": id, "name": name, "category": category, "is_deprecated": false,
            });
            let data = match uri.path() {
                "/tags/19.json" => tag(19, "unclear_name", 4),
                "/tags/20.json" => return StatusCode::TOO_MANY_REQUESTS.into_response(),
                "/tag_aliases.json" => match query.get("search[antecedent_name]").map(|name| name.as_ref()) {
                    Some("old_spelling") => json!([{"antecedent_name":"old_spelling", "consequent_name":"new_spelling", "status":"active"}]),
                    Some("series_old_title") => json!([{"antecedent_name":"series_old_title", "consequent_name":"series", "status":"active"}]),
                    _ => panic!("unexpected alias query"),
                },
                "/tags.json" => match query.get("search[name]").map(|name| name.as_ref()) {
                    Some("new_spelling") => json!([tag(33, "new_spelling", 4)]),
                    Some("old_spelling") => json!([]),
                    Some("series_old_title") => json!([]),
                    Some("series") => json!([tag(34, "series", 3)]),
                    _ => panic!("unexpected tag query"),
                },
                "/wiki_pages/new_spelling.json" => json!({
                    "title":"new_spelling", "is_deleted":false,
                    "body":"The protagonist of [[Series Old Title|Series]].\n\nAppears with a [[character]]."
                }),
                "/wiki_pages/unclear_name.json" => json!({
                    "title":"unclear_name", "is_deleted":false,
                    "body":"A character from [[One]] and protagonist of [[Two]]."
                }),
                _ => panic!("unexpected catalog request"),
            };
            Json(data).into_response()
        });
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let catalog = Catalog {
            client: crate::http::loopback_client().unwrap(),
            base,
        };
        let renamed = catalog
            .identify(None, "old_spelling")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.id, 33);
        let matched = catalog.lookup(&renamed.name).await.unwrap().unwrap();
        assert_eq!(
            matched,
            Match {
                franchise: "series".into(),
                wiki_title: "new_spelling".into()
            }
        );
        let unclear = catalog
            .identify(Some(19), "unclear_name")
            .await
            .unwrap()
            .unwrap();
        assert!(catalog.lookup(&unclear.name).await.unwrap().is_none());
        assert!(
            catalog
                .identify(Some(20), "rate_limited")
                .await
                .unwrap_err()
                .to_string()
                .contains("429")
        );
        let db = Db::open_in_memory().unwrap();
        db.with(|conn| {
            tag_suggestions::cache_character_id(conn, "old_spelling", Some(renamed.id))?;
            tag_suggestions::cache_lookup(
                conn,
                renamed.id,
                Some((&matched.franchise, &matched.wiki_title)),
            )
        })
        .unwrap();
        server.abort();
        let _ = server.await;
        let local = db
            .with(|conn| {
                let id = tag_suggestions::character_id(conn, "old_spelling")?
                    .flatten()
                    .unwrap();
                tag_suggestions::franchise(conn, id)
            })
            .unwrap()
            .unwrap();
        assert_eq!(local.franchise.as_deref(), Some("series"));
        assert_eq!(local.wiki_title.as_deref(), Some("new_spelling"));
    }

    #[test]
    fn ignores_a_franchise_link_without_an_explicit_origin_and_links_after_the_intro() {
        assert!(
            opening_work("Often drawn with [[Series]].\n\nA character from [[Other Series]].")
                .is_none()
        );
        assert!(opening_work("A character from [[One]] and [[Two]].").is_none());
        assert!(opening_work("She is not a character from [[One]].").is_none());
    }
}
