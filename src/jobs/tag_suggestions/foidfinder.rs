//! FoidFinder's local HTTP API. Images are sent as raw request bodies.

use std::num::NonZeroU16;
use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;

use crate::error::{Error, Result};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const RESPONSE_ERROR: &str = "FoidFinder returned an invalid response";

pub(super) struct FoidFinder {
    client: Client,
    port: NonZeroU16,
}

#[derive(Debug)]
pub(super) struct Hit {
    pub name: String,
    pub score: f32,
    pub strong: bool,
}

#[derive(Default)]
pub(super) struct Prediction {
    pub characters: Vec<Hit>,
    pub series: Vec<Hit>,
}

#[derive(Deserialize)]
struct Tag {
    tag: String,
    score: f32,
}

#[derive(Deserialize)]
struct Tags {
    #[serde(default)]
    character: Vec<Tag>,
    #[serde(default)]
    copyright: Vec<Tag>,
}

#[derive(Deserialize)]
struct TagResponse {
    tags: Tags,
    candidates: Tags,
}

impl FoidFinder {
    pub fn new(port: NonZeroU16) -> Result<Self> {
        Ok(Self {
            client: crate::http::loopback_client()?,
            port,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/api/{path}", self.port)
    }

    pub async fn health(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct Health {
            status: String,
        }

        let response = self
            .client
            .get(self.endpoint("health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(connection_error)?;
        if response.status() == StatusCode::OK
            && serde_json::from_slice::<Health>(&read_response(response).await?)
                .is_ok_and(|health| health.status == "ok")
        {
            return Ok(());
        }
        Err(Error::other(
            "FoidFinder is not ready. Check the service log and configured port.",
        ))
    }

    pub async fn predict(&self, bytes: Vec<u8>, deadline: Duration) -> Result<Option<Prediction>> {
        let response = self
            .client
            .post(self.endpoint("tag"))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .timeout(deadline)
            .body(bytes)
            .send()
            .await
            .map_err(connection_error)?;
        match response.status() {
            StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY => Ok(None),
            StatusCode::OK => parse(&read_response(response).await?).map(Some),
            other => Err(Error::other(format!(
                "FoidFinder returned {other}. Check the service log."
            ))),
        }
    }
}

async fn read_response(mut response: Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(connection_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(Error::other("FoidFinder response exceeded the size limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn connection_error(err: reqwest::Error) -> Error {
    if err.is_timeout() {
        Error::Timeout("FoidFinder did not respond. Check the service log.".into())
    } else {
        Error::other("Cannot reach FoidFinder. Start it and check the configured port.")
    }
}

fn parse(body: &[u8]) -> Result<Prediction> {
    let response: TagResponse =
        serde_json::from_slice(body).map_err(|_| Error::other(RESPONSE_ERROR))?;
    let hits = |tags: Vec<Tag>, strong| {
        tags.into_iter().map(move |tag| {
            if tag.tag.trim().is_empty()
                || tag.tag.len() > 256
                || tag.tag.chars().any(char::is_control)
                || !(0.0..=1.0).contains(&tag.score)
            {
                return Err(Error::other(RESPONSE_ERROR));
            }
            Ok(Hit {
                name: tag.tag,
                score: tag.score,
                strong,
            })
        })
    };
    Ok(Prediction {
        characters: hits(response.tags.character, true)
            .chain(hits(response.candidates.character, false))
            .collect::<Result<_>>()?,
        series: hits(response.tags.copyright, true).collect::<Result<_>>()?,
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

    const EMPTY: &str = r#"{"tags":{},"candidates":{}}"#;

    #[tokio::test]
    async fn loopback_prediction_handles_empty_invalid_unreachable_and_slow_responses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = NonZeroU16::new(listener.local_addr().unwrap().port()).unwrap();
        let router = Router::new()
            .route("/api/health", get(|| async { r#"{"status":"ok"}"# }))
            .route("/api/tag", post(|body: Bytes| async move {
                match body.as_ref() {
                    b"no match" => EMPTY.into_response(),
                    b"malformed" => r#"{"tags":{"character":[{"tag":"pikachu","score":1.1}]},"candidates":{}}"#.into_response(),
                    b"unsupported" => StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response(),
                    b"oversized" => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
                    b"slow" => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        EMPTY.into_response()
                    }
                    _ => r#"{"rating":{"tag":"general","score":0.99},
                     "tags":{"character":[{"tag":"pikachu","score":0.36}],"copyright":[{"tag":"pokemon","score":0.29}],"artist":[{"tag":"artist","score":0.9}]},
                     "candidates":{"character":[{"tag":"raichu","score":0.2}],"copyright":[{"tag":"pokemon_go","score":0.06}]}}"#.into_response(),
                }
            }));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let tagger = FoidFinder::new(port).unwrap();
        let predict = |body: &'static [u8]| tagger.predict(body.to_vec(), Duration::from_secs(2));
        tagger.health().await.unwrap();
        let hits = predict(b"image").await.unwrap().unwrap();
        assert_eq!(hits.characters.len(), 2);
        assert_eq!(hits.characters[0].name, "pikachu");
        assert!(hits.characters[0].strong);
        assert!(!hits.characters[1].strong);
        assert_eq!(hits.series.len(), 1);
        assert_eq!(hits.series[0].name, "pokemon");
        assert!(
            predict(b"no match")
                .await
                .unwrap()
                .unwrap()
                .characters
                .is_empty()
        );
        assert!(predict(b"unsupported").await.unwrap().is_none());
        assert!(predict(b"oversized").await.unwrap().is_none());
        assert!(predict(b"malformed").await.is_err());
        assert!(matches!(
            tagger
                .predict(b"slow".to_vec(), Duration::from_millis(25))
                .await,
            Err(Error::Timeout(_)),
        ));
        server.abort();
        let _ = server.await;
        assert!(
            tagger
                .health()
                .await
                .unwrap_err()
                .to_string()
                .contains("Start it")
        );
    }
}
