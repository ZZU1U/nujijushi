use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::*;
use crate::source;
use crate::state::AppState;

const JAR: &str = "#HttpOnly_.x.com\tTRUE\t/\tTRUE\t0\tauth_token\tlocal-auth\n.x.com\tTRUE\t/\tTRUE\t0\tct0\tlocal-csrf\n";
const COOKIE: &str = "auth_token=local-auth; ct0=local-csrf";
const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const PATH: &str = "M 10,30 C16 32 64 16 32 64 0 0 0 255 255";
const INDICES: &str = "parseInt(k[0],16); parseInt(k[0], 16);";

#[derive(Clone)]
struct Fixture {
    modern: bool,
    requests: Arc<Mutex<Vec<(Uri, HeaderMap)>>>,
}

struct Loopback {
    fixture: Fixture,
    origin: url::Url,
    server: tokio::task::JoinHandle<()>,
}

impl Loopback {
    async fn start(modern: bool) -> Self {
        let fixture = Fixture {
            modern,
            requests: Arc::default(),
        };
        let router = Router::new().fallback(respond).with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            fixture,
            origin,
            server,
        }
    }

    fn app(&self, home: &tempfile::TempDir) -> AppState {
        let mut cfg = Config::load_from_home(home.path().to_owned()).unwrap();
        cfg.twitter_backend = TwitterBackend::Native;
        cfg.twitter_test_origin = Some(self.origin.clone());
        cfg.gallery_dl = fake_gallery_dl(home).display().to_string();
        AppState::open(cfg, None).unwrap()
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn png() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn photo(id: u64) -> Value {
    json!({
        "id_str": "1111",
        "type": "photo",
        "media_url_https": format!("https://pbs.twimg.com/media/{id}.png"),
        "original_info": {"width": 8, "height": 8}
    })
}

fn video(id: u64, kind: &str, stream_only: bool) -> Value {
    let mut variants = vec![json!({
        "content_type": "application/x-mpegURL",
        "url": format!("https://video.twimg.com/clip/{id}.m3u8")
    })];
    if !stream_only {
        variants.extend([
            json!({"content_type": "video/mp4", "bitrate": 100, "url": format!("https://video.twimg.com/clip/{id}-low.mp4")}),
            json!({"content_type": "video/mp4", "bitrate": 300, "url": format!("https://video.twimg.com/clip/{id}-{kind}.mp4")}),
        ]);
    }
    json!({
        "id_str": "2222",
        "type": kind,
        "video_info": {"variants": variants},
        "sensitive_media_warning": {"adult_content": true}
    })
}

fn tweet(id: u64, modern: bool) -> Value {
    let mut media = vec![photo(id)];
    if matches!(id, 42 | 49) {
        media.extend([video(id, "video", false), video(id, "animated_gif", false)]);
    }
    if id == 43 {
        media.push(video(id, "video", true));
    }
    if matches!(id, 47 | 48) {
        media.clear();
    }
    let user = json!({"screen_name": "source_artist", "name": "Display Artist", "id_str": "77"});
    let profile = if modern {
        json!({"rest_id":"77", "core":user})
    } else {
        json!({"rest_id":"77", "legacy":user})
    };
    let mut value = json!({
        "__typename":"Tweet",
        "rest_id":id.to_string(),
        "core":{"user_results":{"result":profile}},
        "legacy":{
            "id_str":id.to_string(),
            "user_id_str":"77",
            "full_text":"Legacy text",
            "created_at":"Tue Nov 14 22:13:20 +0000 2023",
            "possibly_sensitive": id == 41,
            "entities":{"hashtags":[{"text":"Legacy"}]},
            "extended_entities":{"media":media},
            "is_quote_status": id == 47
        }
    });
    if id == 48 {
        value["legacy"]["retweeted_status_result"] = json!({"result":{"rest_id":"99"}});
    }
    if modern {
        value["note_tweet"] = json!({"note_tweet_results":{"result":{
            "text":"Long form text",
            "entity_set":{"hashtags":[{"text":"Note"}]}
        }}});
    }
    value
}

async fn respond(State(fixture): State<Fixture>, uri: Uri, headers: HeaderMap) -> Response {
    fixture
        .requests
        .lock()
        .unwrap()
        .push((uri.clone(), headers.clone()));
    let path = uri.path();
    match path {
        "/tesla" => {
            let asset = if fixture.modern {
                r#"<script src="https://abs.twimg.com/x-web/x-web/entry-client-abc.js"></script>"#
            } else {
                r#"<script>const chunks={19:"ondemand.s"};const hashes={19:"1234abc"};</script>"#
            };
            (
                [(header::CONTENT_TYPE, "text/html")],
                format!(r#"<meta content="{KEY}" name="twitter-site-verification"><svg id="loading-x-anim-0"><g><path d="unused"/><path d="{PATH}"/></g></svg>{asset}"#),
            ).into_response()
        }
        "/x-web/x-web/entry-client-abc.js" => r#"import "./assets/app.js";"#.into_response(),
        "/x-web/x-web/assets/app.js" => {
            r#"import("./hash1.js").then(e=>e.default());"#.into_response()
        }
        "/x-web/x-web/assets/hash1.js" | "/responsive-web/client-web/ondemand.s.1234abca.js" => {
            INDICES.into_response()
        }
        _ if path.ends_with("/TweetDetail") => {
            let params: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(uri.query().unwrap().as_bytes()).collect();
            let variables: Value = serde_json::from_str(&params["variables"]).unwrap();
            let id = variables["focalTweetId"].as_str().unwrap().parse().unwrap();
            match id {
                44 => Json(json!({"errors":[{"code":179}]})).into_response(),
                45 => StatusCode::UNAUTHORIZED.into_response(),
                46 => StatusCode::TOO_MANY_REQUESTS.into_response(),
                _ => {
                    let mut quoted = tweet(id, fixture.modern);
                    quoted["legacy"]["extended_entities"]["media"] = json!([photo(999)]);
                    let mut reply = tweet(999, fixture.modern);
                    reply["quoted_status_result"] = json!({"result":quoted});
                    let result = json!({"__typename":"TweetWithVisibilityResults","tweet":tweet(id, fixture.modern)});
                    Json(json!({"data":{"threaded_conversation_with_injections_v2":{"instructions":[
                        {"type":"TimelineAddEntries","entries":[
                            {"entryId":"tweet-999","content":{"itemContent":{"tweet_results":{"result":reply}}}},
                            {"entryId":format!("tweet-{id}"),"content":{"itemContent":{"tweet_results":{"result":result}}}}
                        ]}
                    ]}}})).into_response()
                }
            }
        }
        _ if path.starts_with("/media/") => {
            ([(header::CONTENT_TYPE, "image/png")], png()).into_response()
        }
        "/clip/49-video.mp4" => StatusCode::BAD_GATEWAY.into_response(),
        _ if path.starts_with("/clip/") => {
            ([(header::CONTENT_TYPE, "video/mp4")], "mp4 bytes").into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn fake_gallery_dl(home: &tempfile::TempDir) -> PathBuf {
    let program = home.path().join("fake-gallery-dl");
    std::fs::write(home.path().join("original.png"), png()).unwrap();
    std::fs::write(
        &program,
        r#"#!/bin/sh
set -eu
base=$(dirname "$0")
cookie=
staging=
target=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --cookies) cookie="$2"; shift ;;
        -d) staging="$2"; shift ;;
        http*) target="$1" ;;
    esac
    shift
done
id=$(basename "$target")
printf '%s\n' "$id:$cookie" >> "$base/fallback.log"
cp "$base/original.png" "$staging/image.png"
printf '{"id":"%s","category":"twitter","user":{"id":"77","name":"source_artist","nick":"Display Artist"},"possibly_sensitive":true}' "$id" > "$staging/image.png.json"
"#,
    ).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

#[test]
fn session_accepts_only_current_x_cookies() {
    let (cookie, csrf) = session_headers(JAR, 100).unwrap();
    assert_eq!(cookie, COOKIE);
    assert_eq!(csrf, "local-csrf");
    assert!(cookie.is_sensitive() && csrf.is_sensitive());
    assert!(session_headers(&JAR.replace(".x.com", ".twitter.com"), 100).is_none());
    assert!(session_headers(&JAR.replace("\t0\tct0", "\t100\tct0"), 100).is_none());
    assert!(session_headers(&JAR.replace("local-auth", "bad;cookie"), 100).is_none());
}

#[tokio::test]
async fn legacy_signing_and_backends_update_the_same_archive_record() {
    let loopback = Loopback::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let mut state = loopback.app(&home);
    let target = "https://twitter.com/request_slug/status/41";
    let first = crate::jobs::import_target(&state, target, None)
        .await
        .unwrap();
    assert!(loopback.fixture.requests.lock().unwrap().is_empty());

    std::fs::write(state.cfg.cookies_dir().join("twitter.txt"), JAR).unwrap();
    let watched = source::fetch_watched(&state.cfg, target).await.unwrap();
    assert_eq!(watched[0].source_id, "41");
    assert!(loopback.fixture.requests.lock().unwrap().is_empty());
    source::cleanup_staging(&state.cfg, &watched);
    let second = crate::jobs::import_target(&state, target, None)
        .await
        .unwrap();
    assert_eq!(first.post_ids, second.post_ids);
    assert_eq!(second.updated, 1);
    let post = state
        .db
        .with(|conn| crate::db::posts::get(conn, second.post_ids[0]))
        .unwrap();
    assert_eq!(post.media.len(), 1);
    assert_eq!(post.rating, Rating::Explicit);
    let author = post.author.unwrap();
    assert_eq!(author.source_id, "77");
    assert_eq!(author.name.as_deref(), Some("Display Artist"));
    assert_eq!(author.handle.as_deref(), Some("source_artist"));

    Arc::make_mut(&mut state.cfg).twitter_backend = TwitterBackend::GalleryDl;
    let third = crate::jobs::import_target(&state, target, None)
        .await
        .unwrap();
    assert_eq!(first.post_ids, third.post_ids);
    assert_eq!(third.updated, 1);
    let requests = loopback.fixture.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(url, _)| url.path().contains("ondemand.s"))
    );
    assert!(
        requests
            .iter()
            .any(|(url, _)| url.query() == Some("format=png&name=orig"))
    );
}

#[tokio::test]
async fn modern_signing_preserves_media_order_and_scopes_cookies() {
    let loopback = Loopback::start(true).await;
    let home = tempfile::tempdir().unwrap();
    let state = loopback.app(&home);
    let cookie = state.cfg.cookies_dir().join("x.com.txt");
    std::fs::write(&cookie, JAR).unwrap();
    let invalid_primary = state.cfg.cookies_dir().join("twitter.txt");
    std::fs::write(&invalid_primary, "# Netscape HTTP Cookie File\n").unwrap();
    let invalid = source::fetch_with(&state.cfg, "https://x.com/artist/status/42")
        .await
        .unwrap_err();
    assert!(
        invalid
            .to_string()
            .contains("needs unexpired auth_token and ct0")
    );
    assert!(loopback.fixture.requests.lock().unwrap().is_empty());
    std::fs::remove_file(invalid_primary).unwrap();

    let posts = source::fetch_with(&state.cfg, "https://x.com/artist/status/42/photo/1")
        .await
        .unwrap();
    let post = &posts[0];
    assert_eq!(post.source_id, "42");
    assert_eq!(post.description.as_deref(), Some("Long form text"));
    assert_eq!(
        post.author.as_ref().unwrap().handle.as_deref(),
        Some("source_artist")
    );
    assert!(post.tags.contains(&"note".into()));
    assert_eq!(post.rating, Rating::Explicit);
    assert_eq!(
        post.files
            .iter()
            .map(|file| file.is_video)
            .collect::<Vec<_>>(),
        [false, true, true]
    );
    assert_eq!(
        post.files
            .iter()
            .map(|file| file.sensitive)
            .collect::<Vec<_>>(),
        [false, true, true]
    );
    source::cleanup_staging(&state.cfg, &posts);
    {
        let requests = loopback.fixture.requests.lock().unwrap();
        let media: Vec<_> = requests
            .iter()
            .map(|(uri, _)| uri.path())
            .filter(|path| path.starts_with("/media/") || path.starts_with("/clip/"))
            .collect();
        assert_eq!(
            media,
            [
                "/media/42",
                "/clip/42-video.mp4",
                "/clip/42-animated_gif.mp4"
            ]
        );
        for (uri, headers) in requests.iter() {
            let auth = uri.path() == "/tesla" || uri.path().ends_with("/TweetDetail");
            assert_eq!(headers.contains_key(header::COOKIE), auth, "{uri}");
            if auth {
                assert_eq!(headers[header::COOKIE], COOKIE);
            }
            if uri.path().ends_with("/TweetDetail") {
                assert_eq!(headers["x-csrf-token"], "local-csrf");
                assert!(headers.contains_key("x-client-transaction-id"));
                assert!(headers.contains_key(header::AUTHORIZATION));
            } else {
                assert!(!headers.contains_key("x-csrf-token"));
                assert!(!headers.contains_key(header::AUTHORIZATION));
            }
        }
    }

    for id in [43, 47, 48] {
        let target = format!("https://twitter.com/artist/status/{id}");
        let fallback = source::fetch_with(&state.cfg, &target).await.unwrap();
        assert_eq!(fallback[0].source_id, id.to_string());
        source::cleanup_staging(&state.cfg, &fallback);
    }
    for (id, expected) in [
        (44, "cannot view"),
        (45, "rejected the signed-in session"),
        (46, "rate limit"),
        (49, "X attachment 2 download failed"),
    ] {
        let target = twitter::status_url(id);
        let error = source::fetch_with(&state.cfg, &target).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    let log = std::fs::read_to_string(home.path().join("fallback.log")).unwrap();
    let expected = [43, 47, 48].map(|id| format!("{id}:{}", cookie.display()));
    assert_eq!(log.lines().collect::<Vec<_>>(), expected);
    assert!(
        std::fs::read_dir(state.cfg.staging_dir())
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STAGING_PREFIX)
            })
    );
}
