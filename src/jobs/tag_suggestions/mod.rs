//! Collect image evidence from a local FoidFinder server and enrich it locally.

mod catalog;
mod foidfinder;

use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use self::catalog::Catalog;
use self::foidfinder::{FoidFinder, Hit};
use crate::db::{jobs, tag_suggestions as db};
use crate::error::{Error, Result};
use crate::model::{AssociationOrigin, MediaItem, SuggestionKind};
use crate::state::AppState;

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const IMAGE_LIMIT: usize = 12;
const INFERENCE_DEADLINE: Duration = Duration::from_secs(120);
const LOOKUP_DEADLINE: Duration = Duration::from_secs(15);

pub async fn health(port: NonZeroU16) -> Result<()> {
    FoidFinder::new(port)?.health().await
}

#[derive(Debug)]
struct Detection {
    hit: Hit,
    best_image: i64,
    strong_images: Vec<i64>,
    character_id: Option<i64>,
}

impl Detection {
    fn into_candidate(self, kind: SuggestionKind) -> db::Candidate {
        let low_confidence = !self.hit.strong;
        let (evidence, images) = if low_confidence {
            ("FoidFinder · Low confidence", vec![self.best_image])
        } else {
            ("FoidFinder", self.strong_images)
        };
        db::Candidate {
            kind,
            tag: self.hit.name,
            character_tag_id: self.character_id,
            evidence,
            low_confidence,
            wiki_title: None,
            images,
        }
    }
}

fn rank(hits_by_name: BTreeMap<String, Detection>) -> Vec<Detection> {
    let mut detections: Vec<_> = hits_by_name.into_values().collect();
    detections.sort_by(|a, b| {
        b.hit
            .strong
            .cmp(&a.hit.strong)
            .then_with(|| b.hit.score.total_cmp(&a.hit.score))
            .then_with(|| a.hit.name.cmp(&b.hit.name))
    });
    detections
}

fn collect_hits(hits_by_name: &mut BTreeMap<String, Detection>, image: i64, hits: Vec<Hit>) {
    use std::collections::btree_map::Entry;

    for hit in hits {
        match hits_by_name.entry(hit.name.clone()) {
            Entry::Occupied(mut entry) => {
                let entry = entry.get_mut();
                if hit.strong && entry.strong_images.last() != Some(&image) {
                    entry.strong_images.push(image);
                }
                if (hit.strong, hit.score) > (entry.hit.strong, entry.hit.score) {
                    entry.hit = hit;
                    entry.best_image = image;
                }
            }
            Entry::Vacant(entry) => {
                let strong_images = hit.strong.then_some(image).into_iter().collect();
                entry.insert(Detection {
                    hit,
                    best_image: image,
                    strong_images,
                    character_id: None,
                });
            }
        }
    }
}

struct Scan {
    characters: Vec<Detection>,
    series: Vec<Detection>,
    scanned: usize,
    skipped: usize,
}

async fn scan(state: &AppState, media: &[MediaItem], job_id: i64) -> Result<Scan> {
    let tagger = FoidFinder::new(state.cfg.tag_suggestions.tagger_port)?;
    let media = media
        .iter()
        .enumerate()
        .filter(|(_, media)| !media.is_video)
        .take(IMAGE_LIMIT)
        .collect::<Vec<_>>();
    let total = media.len() as i64;
    let mut scanned = 0;
    let (mut characters, mut series) = (BTreeMap::new(), BTreeMap::new());
    state
        .db
        .call(move |conn| jobs::progress(conn, job_id, 0, total))
        .await?;
    for (index, (position, item)) in media.into_iter().enumerate() {
        let image = position as i64 + 1;
        let path = state.blobs.path_for(&item.blob_hash, &item.ext);
        let data = if item.bytes >= 0 && (item.bytes as u64) <= MAX_IMAGE_BYTES {
            read_image(&path).await
        } else {
            None
        };
        if let Some(bytes) = data
            && let Some(prediction) = tagger.predict(bytes, Duration::from_secs(25)).await?
        {
            scanned += 1;
            collect_hits(&mut characters, image, prediction.characters);
            collect_hits(&mut series, image, prediction.series);
        }
        state
            .db
            .call(move |conn| jobs::progress(conn, job_id, index as i64 + 1, total))
            .await?;
    }
    Ok(Scan {
        characters: rank(characters),
        series: rank(series),
        scanned,
        skipped: total as usize - scanned,
    })
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
    if bytes.is_empty() || bytes.len() as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    // FoidFinder's decoder otherwise uses the first frame of animations.
    match image::guess_format(&bytes) {
        Ok(
            format @ (image::ImageFormat::Gif | image::ImageFormat::Png | image::ImageFormat::WebP),
        ) => tokio::task::spawn_blocking(move || (!animated(&bytes, format)).then_some(bytes))
            .await
            .ok()?,
        _ if bytes.get(4..12) == Some(b"ftypavis") => None,
        _ => Some(bytes),
    }
}

fn animated(bytes: &[u8], format: image::ImageFormat) -> bool {
    use image::codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder};
    use image::{AnimationDecoder, ImageDecoder};
    use std::io::Cursor;

    let reader = Cursor::new(bytes);
    match format {
        image::ImageFormat::Png => PngDecoder::new(reader)
            .and_then(|decoder| decoder.is_apng())
            .unwrap_or(false),
        image::ImageFormat::WebP => {
            WebPDecoder::new(reader).is_ok_and(|decoder| decoder.has_animation())
        }
        image::ImageFormat::Gif => GifDecoder::new(reader).is_ok_and(|mut decoder| {
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(128 * 1024 * 1024);
            decoder.set_limits(limits).is_err() || decoder.into_frames().nth(1).is_some()
        }),
        _ => false,
    }
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
        low_confidence: false,
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

fn from_association(association: db::Association) -> Option<db::Candidate> {
    let evidence = if association.origin == AssociationOrigin::Lookup {
        "Danbooru"
    } else {
        "Local catalog"
    };
    Some(franchise_candidate(
        association.franchise?,
        evidence,
        association.wiki_title,
    ))
}

async fn known_franchises(
    state: &AppState,
    characters: &mut [Detection],
    out: &mut Vec<db::Candidate>,
) -> Result<()> {
    let strong: Vec<_> = characters
        .iter_mut()
        .take_while(|detection| detection.hit.strong)
        .take(5)
        .collect();
    if strong.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = strong
        .iter()
        .map(|detection| detection.hit.name.clone())
        .collect();
    let local = state
        .db
        .call(move |conn| {
            names
                .into_iter()
                .map(|name| {
                    let identity = db::character_id(conn, &name)?;
                    let association = match identity.flatten() {
                        Some(id) => db::franchise(conn, id)?,
                        None => None,
                    };
                    Ok((identity, association))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?;
    let mut unresolved = Vec::new();
    for (detection, (identity, association)) in strong.into_iter().zip(local) {
        detection.character_id = identity.flatten();
        if let Some(association) = association {
            out.extend(from_association(association));
        } else if identity != Some(None) {
            unresolved.push(detection);
        }
    }
    if state.cfg.tag_suggestions.online_fallback && !unresolved.is_empty() {
        match Catalog::new(&state.cfg) {
            Ok(catalog) => {
                let deadline = tokio::time::Instant::now() + LOOKUP_DEADLINE;
                for detection in unresolved.into_iter().take(3) {
                    let lookup = lookup_franchise(state, &catalog, detection, deadline).await?;
                    match lookup {
                        Some(candidate) => out.push(candidate),
                        None if tokio::time::Instant::now() >= deadline => break,
                        None => (),
                    }
                }
            }
            Err(err) => tracing::debug!("franchise lookup: {err}"),
        }
    }
    Ok(())
}

async fn lookup_franchise(
    state: &AppState,
    catalog: &Catalog,
    detection: &mut Detection,
    deadline: tokio::time::Instant,
) -> Result<Option<db::Candidate>> {
    let label = detection.hit.name.clone();
    let id = detection.character_id;
    let character = match tokio::time::timeout_at(deadline, catalog.identify(id, &label)).await {
        Ok(Ok(character)) => character,
        Ok(Err(err)) => {
            tracing::debug!("character lookup: {err}");
            return Ok(None);
        }
        Err(_) => return Ok(None),
    };
    let resolved = character.as_ref().map(|tag| tag.id);
    let association = state
        .db
        .call(move |conn| {
            db::cache_character_id(conn, &label, resolved)?;
            match resolved.or(id) {
                Some(id) if resolved.is_none() => {
                    db::cache_lookup(conn, id, None)?;
                    Ok(None)
                }
                Some(id) => db::franchise(conn, id),
                None => Ok(None),
            }
        })
        .await?;
    let Some(character) = character else {
        return Ok(None);
    };
    detection.character_id = Some(character.id);
    if let Some(association) = association {
        return Ok(from_association(association));
    }
    let matched = match tokio::time::timeout_at(deadline, catalog.lookup(&character.name)).await {
        Ok(Ok(matched)) => matched,
        Ok(Err(err)) => {
            tracing::debug!("franchise lookup: {err}");
            return Ok(None);
        }
        Err(_) => return Ok(None),
    };
    state
        .db
        .call(move |conn| {
            db::cache_lookup(
                conn,
                character.id,
                matched
                    .as_ref()
                    .map(|item| (item.franchise.as_str(), item.wiki_title.as_str())),
            )?;
            Ok(matched
                .map(|item| franchise_candidate(item.franchise, "Danbooru", Some(item.wiki_title))))
        })
        .await
}

pub(super) async fn run(state: &AppState, post_id: i64, job_id: i64) -> Result<super::JobReport> {
    if !state.cfg.tag_suggestions.enabled {
        return Err(Error::invalid("Tag suggestions are disabled"));
    }
    let input = state.db.call(move |conn| db::input(conn, post_id)).await?;
    let Scan {
        mut characters,
        series,
        scanned,
        skipped,
    } = tokio::time::timeout(INFERENCE_DEADLINE, scan(state, &input.post.media, job_id))
        .await
        .map_err(|_| Error::Timeout("FoidFinder analysis exceeded 120 seconds".into()))??;
    let mut franchises = source_franchises(&input);
    known_franchises(state, &mut characters, &mut franchises).await?;
    let candidates: Vec<_> = characters
        .into_iter()
        .map(|detection| detection.into_candidate(SuggestionKind::Character))
        .chain(franchises)
        .chain(
            series
                .into_iter()
                .map(|detection| detection.into_candidate(SuggestionKind::Franchise)),
        )
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
    use image::{Frame, ImageFormat, RgbaImage, codecs::gif::GifEncoder};

    use super::*;

    #[test]
    fn recognizes_animated_gifs_without_excluding_single_frame_gifs() {
        let gif = |frame_count| {
            let mut bytes = Vec::new();
            let frames = (0..frame_count).map(|_| Frame::new(RgbaImage::new(4, 4)));
            GifEncoder::new(&mut bytes).encode_frames(frames).unwrap();
            bytes
        };
        assert!(!animated(&gif(1), ImageFormat::Gif));
        assert!(animated(&gif(2), ImageFormat::Gif));
    }
}
