use std::io::Cursor;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::*;
use crate::state::AppState;
use crate::{jobs, source};

const GALLERY_ID: &str = "674051740000000007027a15";
const VIDEO_ID: &str = "674051740000000007027a16";
const BROKEN_ID: &str = "674051740000000007027a17";
const DIFFERENT_ID: &str = "674051740000000007027a18";
const AUTHOR: &str = "5c31698d0000000007018a31";
const SHARE_LINK: &str = "https://xhslink.com/o/opaque-fixture";
const SHARE_TOKEN: &str = "private-fixture";
const JAR: &str = concat!(
    "#HttpOnly_.rednote.com\tTRUE\t/\tTRUE\t0\tweb_session\trednote-demo\n",
    ".xiaohongshu.com\tTRUE\t/\tTRUE\t0\tweb_session\toriginal-demo\n",
    ".rednote.com\tTRUE\t/explore\tTRUE\t0\tpath_cookie\tonly-explore\n",
    ".rednote.com\tTRUE\t/explore-not-a-match\tTRUE\t0\twrong_path\tdiscard\n",
    ".xhslink.com\tTRUE\t/\tTRUE\t0\tforbidden\tshortener\n",
    ".xhscdn.com\tTRUE\t/\tTRUE\t0\tforbidden\tcdn\n",
);

#[derive(Clone, Default)]
struct Fixture(Arc<Mutex<Vec<(Uri, HeaderMap)>>>);

struct Server {
    fixture: Fixture,
    addr: Url,
    server: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start() -> Self {
        let fixture = Fixture::default();
        let router = Router::new().fallback(respond).with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            fixture,
            addr,
            server,
        }
    }

    fn app(&self, home: &tempfile::TempDir) -> AppState {
        let mut cfg = Config::load_from_home(home.path().to_owned()).unwrap();
        cfg.rednote_test_origin = Some(self.addr.clone());
        cfg.gallery_dl = "/missing-gallery-dl".into();
        AppState::open(cfg, None).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn redirect(location: String) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

fn png(width: u32) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(width, 8, image::Rgb([10, 20, 30]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn photo(url: &str) -> Value {
    json!({
        "infoList": [
            {"url":"https://sns-webpic.xhscdn.com/cdn/preview","imageScene":"WB_PRV"},
            {"url":url,"imageScene":"WB_DFT"}
        ]
    })
}

fn video_note() -> Value {
    json!({
        "type": "video",
        "video": {"media": {"stream": {
            "h264": [
                {"width": 1280, "height": 720, "videoBitrate": 2000, "masterUrl": "https://sns-video.xhscdn.com/cdn/low.mp4"},
                {"width": 1920, "height": 1080, "videoBitrate": 3000, "masterUrl": "https://sns-video.xhscdn.com/cdn/high"},
                {"width": 3840, "height": 2160, "videoBitrate": 5000, "masterUrl": "https://sns-video.xhscdn.com/cdn/stream.m3u8"}
            ]
        }}}
    })
}

fn page(id: &str, mut content: Value) -> Response {
    content["noteId"] = json!(if id == DIFFERENT_ID { GALLERY_ID } else { id });
    content["title"] = json!("RedNote fixture");
    content["desc"] = json!("Image notes with an undefined character }.");
    content["user"] = json!({"userId": AUTHOR, "nickname": "Source Artist"});
    content["tagList"] = json!([{"name":"Artwork"}]);
    content["time"] = json!(1_731_099_600_123_i64);
    let state = json!({"note":{"noteDetailMap":{(id):{"note":content}}}});
    (
        [(header::CONTENT_TYPE, "text/html")],
        format!(
            "<script>window.__INITIAL_STATE__ = {{\"store\":{{\"seen\":new Set([])}},{};</script>",
            state.to_string().trim_start_matches('{')
        ),
    )
        .into_response()
}

async fn respond(State(fixture): State<Fixture>, uri: Uri, headers: HeaderMap) -> Response {
    fixture
        .0
        .lock()
        .unwrap()
        .push((uri.clone(), headers.clone()));
    let segments: Vec<_> = uri.path().trim_matches('/').split('/').collect();
    match segments.as_slice() {
        ["o", "opaque-fixture"] => redirect(format!(
            "https://www.xiaohongshu.com/discovery/item/{GALLERY_ID}?xsec_token={SHARE_TOKEN}&xsec_source=app_share"
        )),
        ["discovery", "item", GALLERY_ID] => {
            redirect(format!("https://www.rednote.com/explore/{GALLERY_ID}"))
        }
        ["a", "loop"] => redirect("/a/loop".into()),
        ["o", "foreign"] => redirect("https://example.test/file".into()),
        ["o", "downgrade"] => redirect(format!("http://www.rednote.com/explore/{GALLERY_ID}")),
        ["o", "expired"] => redirect("https://www.rednote.com/404?error_code=300031".into()),
        ["o", "limited"] => StatusCode::TOO_MANY_REQUESTS.into_response(),
        ["explore", VIDEO_ID] => {
            if !headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|cookies| cookies.contains("web_session=rednote-demo"))
            {
                return redirect("/login".into());
            }
            page(VIDEO_ID, video_note())
        }
        ["explore", id] | ["user", "profile", AUTHOR, id] => {
            let second = if *id == BROKEN_ID { "broken" } else { "second" };
            page(
                id,
                json!({"type":"normal","imageList":[
                    photo("http://sns-web-i10.rednotecdn.com/cdn/first"),
                    photo(&format!("https://sns-webpic.xhscdn.com/cdn/{second}")),
                ]}),
            )
        }
        ["cdn", "first"] => ([(header::CONTENT_TYPE, "image/png")], png(8)).into_response(),
        ["cdn", "second"] => ([(header::CONTENT_TYPE, "image/png")], png(11)).into_response(),
        ["cdn", "broken"] => StatusCode::BAD_GATEWAY.into_response(),
        ["cdn", "high"] => ([(header::CONTENT_TYPE, "video/mp4")], "MP4 fixture").into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::test]
async fn gallery_import_preserves_order_and_identity_across_urls() {
    let server = Server::start().await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    let first = jobs::import_target(&state, SHARE_LINK, None).await.unwrap();
    let direct = format!(
        "https://www.xiaohongshu.com/user/profile/{AUTHOR}/{GALLERY_ID}?xsec_token=second-fixture"
    );
    let second = jobs::import_target(&state, &direct, None).await.unwrap();
    assert_eq!(first.post_ids, second.post_ids);
    assert_eq!(second.updated, 1);
    let post = state
        .db
        .with(|conn| crate::db::posts::get(conn, second.post_ids[0]))
        .unwrap();
    assert_eq!(post.source, "rednote");
    assert_eq!(post.source_id, GALLERY_ID);
    assert_eq!(post.title.as_deref(), Some("RedNote fixture"));
    assert_eq!(
        post.description.as_deref(),
        Some("Image notes with an undefined character }.")
    );
    assert_eq!(post.tags, ["artwork"]);
    assert_eq!(post.posted_at.unwrap().timestamp(), 1_731_099_600);
    let author = post.author.unwrap();
    assert_eq!(author.source_id, AUTHOR);
    assert_eq!(author.name.as_deref(), Some("Source Artist"));
    assert!(
        post.url
            .unwrap()
            .ends_with(&format!("/explore/{GALLERY_ID}"))
    );
    assert_eq!(
        post.media.iter().map(|file| file.width).collect::<Vec<_>>(),
        [Some(8), Some(11)]
    );
    assert!(
        server
            .fixture
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|(_, headers)| !headers.contains_key(header::COOKIE))
    );
}

#[tokio::test]
async fn signed_requests_scope_cookies_and_fetch_the_best_direct_video() {
    let server = Server::start().await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    let target = format!("https://www.rednote.com/explore/{VIDEO_ID}");
    let error = source::fetch_with(&state.cfg, &target).await.unwrap_err();
    assert!(error.to_string().contains("authorized session"));
    server.fixture.0.lock().unwrap().clear();
    std::fs::write(state.cfg.cookies_dir().join(COOKIE_PATH), JAR).unwrap();
    for url in [SHARE_LINK, &target] {
        let posts = source::fetch_with(&state.cfg, url).await.unwrap();
        if url == target {
            assert_eq!(posts[0].files.len(), 1);
            assert!(posts[0].files[0].is_video);
            assert_eq!(posts[0].files[0].path.extension().unwrap(), "mp4");
        }
        source::cleanup_staging(&state.cfg, &posts);
    }
    let requests = server.fixture.0.lock().unwrap();
    for (uri, headers) in requests.iter() {
        let path = uri.path();
        if path.starts_with("/discovery/") {
            assert_eq!(headers[header::COOKIE], "web_session=original-demo");
        } else if path.starts_with("/explore/") {
            assert_eq!(
                headers[header::COOKIE],
                "path_cookie=only-explore; web_session=rednote-demo"
            );
        } else {
            assert!(!headers.contains_key(header::COOKIE), "{path}");
        }
        if path.ends_with(GALLERY_ID) {
            assert!(uri.query().unwrap().contains("xsec_token=private-fixture"));
            assert!(uri.query().unwrap().contains("xsec_source=app_share"));
        }
        if path.starts_with("/cdn/") {
            assert!(
                !headers[header::REFERER]
                    .to_str()
                    .unwrap()
                    .contains(SHARE_TOKEN)
            );
        }
    }
    assert!(requests.iter().any(|(uri, _)| uri.path() == "/cdn/high"));
    assert!(!requests.iter().any(|(uri, _)| matches!(
        uri.path(),
        "/cdn/low.mp4" | "/cdn/stream.m3u8" | "/cdn/preview"
    )));
}

#[tokio::test]
async fn invalid_links_and_later_download_failures_leave_no_partial_gallery() {
    let server = Server::start().await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    for (target, expected) in [
        ("https://xhslink.com/a/loop".into(), "redirect limit"),
        ("https://xhslink.com/o/foreign".into(), "redirected outside"),
        (
            "https://xhslink.com/o/downgrade".into(),
            "redirected outside",
        ),
        ("https://xhslink.com/o/expired".into(), "fresh share link"),
        (
            "https://xhslink.com/o/limited".into(),
            "rate limit exceeded",
        ),
        (
            format!("https://www.rednote.com/explore/{DIFFERENT_ID}"),
            "different note",
        ),
        (
            format!("https://www.rednote.com/explore/{BROKEN_ID}?xsec_token={SHARE_TOKEN}"),
            "attachment 2 download failed",
        ),
    ] {
        let error = jobs::import_target(&state, &target, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains(SHARE_TOKEN));
    }
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
    assert!(
        state
            .db
            .with(|conn| crate::db::posts::find_by_source_id(conn, "rednote", BROKEN_ID))
            .unwrap()
            .is_none()
    );
}
