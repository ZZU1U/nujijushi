//! Collect image evidence from the standalone WD Tagger and enrich it locally.

mod catalog;

use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use tokio::io::AsyncReadExt;

use self::catalog::Catalog;
use crate::db::{jobs, tag_suggestions as db};
use crate::error::{Error, Result};
use crate::model::{AssociationOrigin, MediaItem, SuggestionKind};
use crate::state::AppState;

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const IMAGE_LIMIT: usize = 12;
const INFERENCE_DEADLINE: Duration = Duration::from_secs(120);
const LOOKUP_DEADLINE: Duration = Duration::from_secs(15);

pub async fn health(port: NonZeroU16) -> Result<()> {
    let response = crate::http::loopback_client()?
        .get(endpoint(port, "health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(connection_error)?;
    if response.status() != StatusCode::NO_CONTENT {
        return Err(Error::other(
            "WD Tagger is not ready. Check the companion log.",
        ));
    }
    Ok(())
}

fn endpoint(port: NonZeroU16, path: &str) -> String {
    format!("http://127.0.0.1:{port}/v1/{path}")
}

fn connection_error(err: reqwest::Error) -> Error {
    if err.is_timeout() {
        Error::Timeout("WD Tagger did not respond. Check the companion log.".into())
    } else {
        Error::other(
            "Cannot reach the WD Tagger companion. Start it and check the configured port.",
        )
    }
}

#[derive(Debug)]
struct Hit {
    id: i64,
    name: String,
    score: f32,
}

fn parse_hits(body: &[u8]) -> Result<Vec<Hit>> {
    const CONTENT_ERROR: &str = "WD Tagger returned an invalid prediction response";
    let text = std::str::from_utf8(body).map_err(|_| Error::other(CONTENT_ERROR))?;
    let mut hits = Vec::new();
    for line in text.lines() {
        let mut columns = line.split('\t');
        let id: Option<i64> = columns.next().and_then(|value| value.parse().ok());
        let Some((id, name, score)) = id.zip(columns.next()).and_then(|(id, name)| {
            let score: f32 = columns.next()?.parse().ok()?;
            (id > 0
                && !name.trim().is_empty()
                && name.len() <= 256
                && !name.chars().any(char::is_control)
                && (0.0..=1.0).contains(&score)
                && columns.next().is_none())
            .then_some((id, name, score))
        }) else {
            return Err(Error::other(CONTENT_ERROR));
        };
        if score > 0.85 {
            hits.push(Hit {
                id,
                name: name.to_owned(),
                score,
            });
        }
    }
    Ok(hits)
}

async fn predict(
    client: &Client,
    url: &str,
    bytes: Vec<u8>,
    deadline: Duration,
) -> Result<Option<Vec<Hit>>> {
    let mut response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .timeout(deadline)
        .body(bytes)
        .send()
        .await
        .map_err(connection_error)?;
    match response.status() {
        StatusCode::PAYLOAD_TOO_LARGE | StatusCode::UNPROCESSABLE_ENTITY => return Ok(None),
        StatusCode::OK => (),
        other => {
            return Err(Error::other(format!(
                "WD Tagger returned {other}. Check the companion log."
            )));
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(connection_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(Error::other("WD Tagger response exceeded the size limit"));
        }
        body.extend_from_slice(&chunk);
    }
    parse_hits(&body).map(Some)
}

#[derive(Debug)]
struct Character {
    hit: Hit,
    images: Vec<i64>,
}

async fn scan(
    state: &AppState,
    media: &[MediaItem],
    job_id: i64,
) -> Result<(Vec<Character>, usize, usize)> {
    let client = crate::http::loopback_client()?;
    let url = endpoint(state.cfg.tag_suggestions.tagger_port, "predict");
    let media = media
        .iter()
        .enumerate()
        .filter(|(_, media)| !media.is_video)
        .take(IMAGE_LIMIT)
        .collect::<Vec<_>>();
    let total = media.len() as i64;
    let mut scanned = 0;
    let mut characters: BTreeMap<i64, Character> = BTreeMap::new();
    state
        .db
        .call(move |conn| jobs::progress(conn, job_id, 0, total))
        .await?;
    for (index, (position, item)) in media.into_iter().enumerate() {
        let image_number = position as i64 + 1;
        let path = state.blobs.path_for(&item.blob_hash, &item.ext);
        let data = if item.bytes >= 0 && (item.bytes as u64) <= MAX_IMAGE_BYTES {
            read_image(&path).await
        } else {
            None
        };
        if let Some(bytes) = data
            && let Some(hits) = predict(&client, &url, bytes, Duration::from_secs(25)).await?
        {
            scanned += 1;
            for hit in hits {
                use std::collections::btree_map::Entry;
                match characters.entry(hit.id) {
                    Entry::Occupied(mut entry) => {
                        let entry = entry.get_mut();
                        if entry.images.last() != Some(&image_number) {
                            entry.images.push(image_number);
                        }
                        if hit.score > entry.hit.score {
                            entry.hit = hit;
                        }
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(Character {
                            hit,
                            images: vec![image_number],
                        });
                    }
                }
            }
        }
        state
            .db
            .call(move |conn| jobs::progress(conn, job_id, index as i64 + 1, total))
            .await?;
    }
    let mut characters: Vec<_> = characters.into_values().collect();
    characters.sort_by(|a, b| {
        b.hit
            .score
            .total_cmp(&a.hit.score)
            .then(a.hit.id.cmp(&b.hit.id))
    });
    Ok((characters, scanned, total as usize - scanned))
}

async fn read_image(path: &std::path::Path) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await
        .ok()?
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    (!bytes.is_empty() && bytes.len() as u64 <= MAX_IMAGE_BYTES).then_some(bytes)
}

fn franchise_candidate(
    tag: String,
    evidence: &'static str,
    wiki_title: Option<String>,
) -> db::Candidate {
    db::Candidate {
        kind: SuggestionKind::Franchise,
        tag,
        character_tag_id: None,
        evidence,
        wiki_title,
        images: Vec::new(),
    }
}

fn source_franchises(input: &db::Input) -> Vec<db::Candidate> {
    std::iter::once(input.metadata.as_deref())
        .chain(
            input
                .post
                .media
                .iter()
                .map(|media| media.meta_json.as_deref()),
        )
        .flatten()
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .flat_map(|metadata| crate::source::normalize::copyright_tags(&metadata))
        .map(|tag| franchise_candidate(tag, "Source metadata", None))
        .collect()
}

async fn known_franchises(
    state: &AppState,
    characters: &[Character],
    out: &mut Vec<db::Candidate>,
) -> Result<()> {
    let ids: Vec<_> = characters
        .iter()
        .take(5)
        .map(|character| character.hit.id)
        .collect();
    let associations = state
        .db
        .call(move |conn| {
            ids.into_iter()
                .map(|id| db::franchise(conn, id))
                .collect::<Result<Vec<_>>>()
        })
        .await?;
    let mut unresolved = Vec::new();
    for (character, association) in characters.iter().zip(associations) {
        match association {
            Some(association) => {
                if let Some(franchise) = association.franchise {
                    let evidence = if association.origin == AssociationOrigin::Lookup {
                        "Danbooru"
                    } else {
                        "Local catalog"
                    };
                    out.push(franchise_candidate(
                        franchise,
                        evidence,
                        association.wiki_title,
                    ));
                }
            }
            None => unresolved.push(&character.hit),
        }
    }
    if state.cfg.tag_suggestions.online_fallback && !unresolved.is_empty() {
        match Catalog::new(&state.cfg) {
            Ok(catalog) => {
                let start = tokio::time::Instant::now();
                for hit in unresolved.into_iter().take(3) {
                    let remaining = LOOKUP_DEADLINE.saturating_sub(start.elapsed());
                    let lookup =
                        tokio::time::timeout(remaining, catalog.lookup(hit.id, &hit.name)).await;
                    let matched = match lookup {
                        Ok(Ok(matched)) => matched,
                        Ok(Err(err)) => {
                            tracing::debug!("franchise lookup: {err}");
                            break;
                        }
                        Err(_) => break,
                    };
                    let cache_value = matched
                        .as_ref()
                        .map(|item| (item.franchise.clone(), item.wiki_title.clone()));
                    let id = hit.id;
                    state
                        .db
                        .call(move |conn| {
                            db::cache_lookup(
                                conn,
                                id,
                                cache_value
                                    .as_ref()
                                    .map(|(tag, wiki)| (tag.as_str(), wiki.as_str())),
                            )
                        })
                        .await?;
                    if let Some(matched) = matched {
                        out.push(franchise_candidate(
                            matched.franchise,
                            "Danbooru",
                            Some(matched.wiki_title),
                        ));
                    }
                }
            }
            Err(err) => tracing::debug!("franchise lookup: {err}"),
        }
    }
    Ok(())
}

pub(super) async fn run(state: &AppState, post_id: i64, job_id: i64) -> Result<super::JobReport> {
    if !state.cfg.tag_suggestions.enabled {
        return Err(Error::invalid("Tag suggestions are disabled"));
    }
    let input = state.db.call(move |conn| db::input(conn, post_id)).await?;
    let (characters, scanned, skipped) =
        tokio::time::timeout(INFERENCE_DEADLINE, scan(state, &input.post.media, job_id))
            .await
            .map_err(|_| Error::Timeout("WD Tagger analysis exceeded 120 seconds".into()))??;
    let mut franchises = source_franchises(&input);
    known_franchises(state, &characters, &mut franchises).await?;
    let candidates: Vec<_> = characters
        .into_iter()
        .map(|character| db::Candidate {
            kind: SuggestionKind::Character,
            tag: character.hit.name,
            character_tag_id: Some(character.hit.id),
            evidence: "WD Tagger",
            wiki_title: None,
            images: character.images,
        })
        .chain(franchises)
        .collect();
    let message = state
        .db
        .call(move |conn| {
            db::finish(
                conn,
                post_id,
                job_id,
                &input.fingerprint,
                &candidates,
                scanned,
                skipped,
            )
        })
        .await?;
    Ok(super::JobReport::Done {
        message,
        post_id: Some(post_id),
    })
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Bytes,
        response::IntoResponse,
        routing::{get, post},
    };

    use super::*;

    #[tokio::test]
    async fn loopback_prediction_handles_empty_invalid_unreachable_and_slow_responses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = NonZeroU16::new(listener.local_addr().unwrap().port()).unwrap();
        let router = Router::new()
            .route("/v1/health", get(|| async { StatusCode::NO_CONTENT }))
            .route(
                "/v1/predict",
                post(|body: Bytes| async move {
                    match body.as_ref() {
                        b"no match" => "".into_response(),
                        b"malformed" => "1\tcharacter\tNaN\n".into_response(),
                        b"unsupported" => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
                        b"slow" => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            "".into_response()
                        }
                        _ => "3861\tpikachu\t0.98\n12\tother\t0.85\n".into_response(),
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = crate::http::loopback_client().unwrap();
        let url = endpoint(port, "predict");
        let predict_body =
            |body: &'static [u8]| predict(&client, &url, body.to_vec(), Duration::from_secs(2));
        health(port).await.unwrap();
        let hits = predict_body(b"image").await.unwrap().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 3861);
        assert!(predict_body(b"no match").await.unwrap().unwrap().is_empty());
        assert!(predict_body(b"unsupported").await.unwrap().is_none());
        assert!(predict_body(b"malformed").await.is_err());
        assert!(matches!(
            predict(&client, &url, b"slow".to_vec(), Duration::from_millis(25)).await,
            Err(Error::Timeout(_)),
        ));
        server.abort();
        let _ = server.await;
        assert!(
            health(port)
                .await
                .unwrap_err()
                .to_string()
                .contains("Start it")
        );
    }
}
