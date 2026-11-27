use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::DateTime;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use super::*;
use crate::config::{Config, PixivBackend};
use crate::model::{PostState, Rating};

const SESSION: &str = "PHPSESSID=test-session";

struct Artwork {
    time: DateTime<Utc>,
    kind: u8,
    pages: usize,
    restriction: u8,
}

impl Artwork {
    fn from_time(time: DateTime<Utc>) -> Self {
        Self {
            time,
            kind: 0,
            pages: 1,
            restriction: 0,
        }
    }
}

#[derive(Default)]
struct Fixture {
    artworks: BTreeMap<u64, Artwork>,
    fail_downloads: BTreeSet<(u64, usize)>,
    index_error: Option<StatusCode>,
    incomplete_index: bool,
    omit_summary: Option<u64>,
    remove_during_summary: Option<u64>,
    summary_batches: Vec<Vec<u64>>,
    requests: Vec<(Uri, HeaderMap)>,
}

struct TestServer {
    fixture: Arc<Mutex<Fixture>>,
    origin: url::Url,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn start(fixture: Fixture) -> Self {
        let fixture = Arc::new(Mutex::new(fixture));
        let router = Router::new().fallback(respond).with_state(fixture.clone());
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", socket.local_addr().unwrap())
            .parse()
            .unwrap();
        Self {
            origin,
            fixture,
            task: tokio::spawn(async move { axum::serve(socket, router).await.unwrap() }),
        }
    }

    fn app(&self, home: &TempDir) -> AppState {
        let mut cfg = Config::load_from_home(home.path().to_path_buf()).unwrap();
        cfg.pixiv_backend = PixivBackend::Native;
        cfg.pixiv_test_origin = Some(self.origin.clone());
        cfg.gallery_dl = home.path().join("fake-gallery-dl").display().to_string();
        AppState::open(cfg, None).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn envelope(body: Value) -> Response {
    Json(json!({"error": false, "body": body})).into_response()
}

fn original(id: u64, index: usize) -> Option<String> {
    (id != 13).then(|| format!("https://i.pximg.net/img-original/{id}_p{index}.png"))
}

fn png(index: usize) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(8 + index as u32, 8, image::Rgb([10, 20, 30]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

async fn respond(
    State(fixture): State<Arc<Mutex<Fixture>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let mut fixture = fixture.lock().unwrap();
    fixture.requests.push((uri.clone(), headers.clone()));
    let path: Vec<_> = uri.path().trim_start_matches('/').split('/').collect();
    match path.as_slice() {
        ["ajax", "user", "7", "profile", "all"] => {
            if let Some(status) = fixture.index_error {
                return status.into_response();
            }
            if fixture.incomplete_index {
                return envelope(json!({"illusts": []}));
            }
            let (manga, illusts): (Vec<_>, Vec<_>) = fixture
                .artworks
                .iter()
                .partition(|(_, artwork)| artwork.kind == 1);
            let index = |works: Vec<(&u64, &Artwork)>| {
                if works.is_empty() {
                    return json!([]);
                }
                Value::Object(
                    works
                        .into_iter()
                        .map(|(id, _)| (id.to_string(), Value::Null))
                        .collect(),
                )
            };
            envelope(json!({"manga": index(manga), "illusts": index(illusts)}))
        }
        ["ajax", "user", "7", "profile", "illusts"] => {
            let ids: Vec<u64> = url::form_urlencoded::parse(uri.query().unwrap().as_bytes())
                .filter(|(key, _)| key == "ids[]")
                .map(|(_, id)| id.parse().unwrap())
                .collect();
            fixture.summary_batches.push(ids.clone());
            if let Some(id) = fixture.remove_during_summary.take() {
                fixture.artworks.remove(&id);
            }
            let works: Map<String, Value> = ids
                .into_iter()
                .filter(|id| fixture.omit_summary != Some(*id))
                .filter_map(|id| {
                    let artwork = fixture.artworks.get(&id)?;
                    Some((
                        id.to_string(),
                        json!({
                            "id": id.to_string(),
                            "userId": "7",
                            "createDate": artwork.time,
                        }),
                    ))
                })
                .collect();
            envelope(json!({"works": works}))
        }
        ["ajax", "illust", "14"] => envelope(json!({"unexpected": true})),
        ["ajax", "illust", "16"] => StatusCode::TOO_MANY_REQUESTS.into_response(),
        ["ajax", "illust", id, tail @ ..] => {
            let id = id.parse::<u64>().unwrap();
            let Some(artwork) = fixture.artworks.get(&id) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if artwork.restriction > 0
                && headers.get(header::COOKIE).and_then(|h| h.to_str().ok()) != Some(SESSION)
            {
                return StatusCode::FORBIDDEN.into_response();
            }
            if tail == ["pages"] {
                let pages: Vec<_> = (0..artwork.pages)
                    .map(|index| {
                        json!({
                            "urls": {"original": original(id, index)},
                            "height": 8,
                            "width": 8 + index,
                        })
                    })
                    .collect();
                return envelope(json!(pages));
            }
            envelope(json!({
                "id": id.to_string(),
                "illustType": artwork.kind,
                "pageCount": artwork.pages,
                "xRestrict": artwork.restriction,
                "userId": "7",
                "userName": "Artist Display Name",
                "userAccount": "artist_handle",
                "title": "Work title #Example",
                "description": "A &amp; B",
                "createDate": artwork.time,
                "urls": {"original": original(id, 0)},
                "width": 8,
                "height": 8,
                "tags": {"tags": [{"tag": "Original"}, {"tag": "日本語"}]},
            }))
        }
        ["img-original", name] => {
            let (id, suffix) = name.split_once("_p").unwrap();
            let id = id.parse().unwrap();
            let page = suffix.strip_suffix(".png").unwrap().parse().unwrap();
            if fixture.fail_downloads.contains(&(id, page)) {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            ([(header::CONTENT_TYPE, "image/png")], png(page)).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn write_gallery_dl(home: &TempDir) {
    std::fs::write(home.path().join("fixture.png"), png(0)).unwrap();
    let program = home.path().join("fake-gallery-dl");
    std::fs::write(
        &program,
        r#"#!/bin/sh
if [ "$1" = "--dump-json" ]; then
    exec cat "${0%/*}/watch-list.json"
fi
while [ "$#" -gt 1 ]; do
    if [ "$1" = "-d" ]; then
        out="$2"
        shift
    fi
    shift
done
id="${1##*/}"
mkdir -p "$out/post"
cp "${0%/*}/fixture.png" "$out/post/001.png"
printf '{"category":"pixiv","id":"%s","user":{"id":7,"name":"Artist Display Name","account":"artist_handle"},"x_restrict":0}' "$id" > "$out/post/001.png.json"
"#,
    )
    .unwrap();
    std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn date(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_750_000_000 + seconds, 0).unwrap()
}

#[tokio::test]
async fn native_pixiv_imports_originals_and_preserves_archive_identity() {
    let mut fixture = Fixture::default();
    for id in [10, 11, 12, 13, 15] {
        fixture.artworks.insert(id, Artwork::from_time(date(0)));
    }
    let manga = fixture.artworks.get_mut(&11).unwrap();
    manga.kind = 1;
    manga.pages = 2;
    manga.restriction = 1;
    fixture.artworks.get_mut(&12).unwrap().restriction = 2;
    fixture.artworks.get_mut(&15).unwrap().kind = 2;
    let server = TestServer::start(fixture).await;
    let home = tempfile::tempdir().unwrap();
    let mut state = server.app(&home);
    write_gallery_dl(&home);

    let url = |id| format!("https://www.pixiv.net/artworks/{id}");
    let first = crate::jobs::import_target(
        &state,
        "https://www.pixiv.net/member_illust.php?illust_id=10",
        None,
    )
    .await
    .unwrap();
    let auth_error = source::fetch_with(&state.cfg, &url(11)).await.unwrap_err();
    assert!(auth_error.to_string().contains("valid session"));
    std::fs::write(
        state.cfg.cookies_dir().join("pixiv.txt"),
        "#HttpOnly_.pixiv.net\tTRUE\t/\tTRUE\t0\tPHPSESSID\ttest-session\n",
    )
    .unwrap();

    let manga = crate::jobs::import_target(&state, &url(11), None)
        .await
        .unwrap();
    let post = state
        .db
        .with(|conn| posts::get(conn, manga.post_ids[0]))
        .unwrap();
    assert_eq!(post.source_id, "11");
    assert_eq!(post.url.as_deref(), Some(url(11).as_str()));
    assert_eq!(post.rating, Rating::Questionable);
    assert_eq!(post.posted_at, Some(date(0)));
    assert!(post.tags.iter().any(|tag| tag == "日本語"));
    let artist = post.author.unwrap();
    assert_eq!(artist.source_id, "7");
    assert_eq!(artist.handle.as_deref(), Some("artist_handle"));
    assert_eq!(artist.name.as_deref(), Some("Artist Display Name"));
    assert_eq!(post.media.len(), 2);
    assert_eq!(post.media[0].width, Some(8));
    assert_eq!(post.media[1].width, Some(9));

    let restricted = source::fetch_with(&state.cfg, &url(12)).await.unwrap();
    assert_eq!(restricted[0].rating, Rating::Explicit);
    source::cleanup_staging(&state.cfg, &restricted);

    for (id, message) in [
        (13, "original artwork pages"),
        (14, "invalid or incomplete artwork detail"),
        (16, "rate limit exceeded"),
    ] {
        let error = crate::jobs::import_target(&state, &url(id), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    let animation = source::fetch_watched(&state.cfg, &url(15)).await.unwrap();
    assert_eq!(animation[0].source_id, "15");
    assert!(
        animation[0].files[0]
            .path
            .to_string_lossy()
            .contains("gdl-")
    );
    source::cleanup_staging(&state.cfg, &animation);

    Arc::make_mut(&mut state.cfg).pixiv_backend = PixivBackend::GalleryDl;
    let second = crate::jobs::import_target(&state, &url(10), None)
        .await
        .unwrap();
    assert_eq!(second.post_ids, first.post_ids);
    assert_eq!(second.updated, 1);
    assert!(
        std::fs::read_dir(state.cfg.staging_dir())
            .unwrap()
            .all(|entry| !entry.unwrap().file_type().unwrap().is_dir())
    );

    let fixture = server.fixture.lock().unwrap();
    let mut signed_metadata = false;
    for (uri, headers) in &fixture.requests {
        if uri.path().starts_with("/img-original/") {
            assert!(!headers.contains_key(header::COOKIE));
            assert!(
                headers[header::REFERER]
                    .to_str()
                    .unwrap()
                    .starts_with("https://www.pixiv.net/artworks/")
            );
        } else if uri.path() == "/ajax/illust/11/pages" {
            signed_metadata = headers[header::COOKIE] == SESSION;
        }
    }
    assert!(signed_metadata);
}

async fn run_watch(state: &AppState) -> Result<JobReport> {
    let (id, created) = crate::jobs::create_watch_check_job(state, 1).await.unwrap();
    assert!(created);
    let job = state
        .db
        .with(|conn| {
            jobs::set_state(conn, id, "running", None)?;
            Ok(jobs::get(conn, id)?.unwrap())
        })
        .unwrap();
    crate::jobs::run_job(state, &job).await
}

fn create_watch(state: &AppState, initial_batch: u16) {
    state
        .db
        .with(|conn| {
            watches::create(
                conn,
                "Artist",
                "https://www.pixiv.net/users/7/artworks",
                initial_batch,
            )
        })
        .unwrap();
}

fn stored_id(state: &AppState, id: &str) -> Option<i64> {
    state
        .db
        .with(|conn| posts::find_by_source_id(conn, "pixiv", id))
        .unwrap()
}

fn watch(state: &AppState) -> watches::Watch {
    state.db.with(|conn| watches::get(conn, 1)).unwrap()
}

#[tokio::test]
async fn pixiv_watch_author_lookup_only_fetches_artwork_metadata() {
    let mut fixture = Fixture::default();
    let mut animation = Artwork::from_time(date(10));
    animation.kind = 2;
    fixture.artworks.insert(15, animation);
    let server = TestServer::start(fixture).await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);

    let artist = source::pixiv::watch_artist(&state.cfg, 15).await.unwrap();
    assert_eq!(artist.id, 7);
    assert_eq!(artist.name, "Artist Display Name");
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(fixture.requests.len(), 1);
    assert_eq!(fixture.requests[0].0.path(), "/ajax/illust/15");
}

#[tokio::test]
async fn native_pixiv_watch_preserves_checkpoints_and_retries_downloads() {
    let mut fixture = Fixture::default();
    for id in 1..=50 {
        fixture.artworks.insert(id, Artwork::from_time(date(0)));
    }
    // The most recently published work has the smallest numeric ID.
    let manga = fixture.artworks.get_mut(&1).unwrap();
    manga.kind = 1;
    manga.time = date(10);
    let server = TestServer::start(fixture).await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    create_watch(&state, 1);

    run_watch(&state).await.unwrap();
    assert_eq!(
        server.fixture.lock().unwrap().summary_batches,
        [(1..=48).collect::<Vec<_>>(), vec![49, 50],]
    );
    let first_id = stored_id(&state, "1").unwrap();
    let first = state.db.with(|conn| posts::get(conn, first_id)).unwrap();
    assert_eq!(first.state, PostState::New);
    assert_eq!(watch(&state).checkpoint, Some(date(10)));
    assert!(stored_id(&state, "50").is_none());

    server.fixture.lock().unwrap().requests.clear();
    run_watch(&state).await.unwrap();
    {
        let fixture = server.fixture.lock().unwrap();
        assert_eq!(fixture.requests.len(), 1);
        assert_eq!(fixture.requests[0].0.path(), "/ajax/user/7/profile/all");
    }

    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.artworks.insert(51, Artwork::from_time(date(10)));
        let mut manga = Artwork::from_time(date(20));
        manga.kind = 1;
        manga.pages = 2;
        fixture.artworks.insert(52, manga);
        fixture.fail_downloads.insert((52, 1));
    }
    assert!(
        run_watch(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("download failed")
    );
    assert!(stored_id(&state, "51").is_some());
    assert!(stored_id(&state, "52").is_none());
    assert_eq!(watch(&state).checkpoint, Some(date(20)));
    assert_eq!(server.fixture.lock().unwrap().summary_batches[2], [51, 52]);
    let pending = state
        .db
        .with(|conn| watches::pending(conn, 1, 25, true))
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, "52");
    assert!(
        std::fs::read_dir(state.cfg.staging_dir())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("pixiv-"))
    );

    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.artworks.insert(53, Artwork::from_time(date(30)));
        fixture.omit_summary = Some(53);
        fixture.fail_downloads.clear();
    }
    assert!(
        run_watch(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("creator summaries")
    );
    assert!(stored_id(&state, "52").is_some());
    assert!(stored_id(&state, "53").is_none());
    assert_eq!(watch(&state).checkpoint, Some(date(20)));
    assert_eq!(
        server.fixture.lock().unwrap().summary_batches[3..],
        [vec![53], vec![53],]
    );

    server.fixture.lock().unwrap().omit_summary = None;
    run_watch(&state).await.unwrap();
    assert!(stored_id(&state, "53").is_some());
    assert_eq!(watch(&state).checkpoint, Some(date(30)));
}

#[tokio::test]
async fn empty_pixiv_baseline_discovers_older_works_in_bounded_batches() {
    let server = TestServer::start(Fixture::default()).await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    create_watch(&state, 1);

    run_watch(&state).await.unwrap();
    let baseline = watch(&state);
    assert!(baseline.pixiv_baselined);
    assert!(baseline.checkpoint.unwrap() > date(0));
    {
        let mut fixture = server.fixture.lock().unwrap();
        assert!(fixture.summary_batches.is_empty());
        fixture
            .artworks
            .extend((101..=149).map(|id| (id, Artwork::from_time(date(0)))));
    }

    let report = run_watch(&state).await.unwrap().message();
    assert!(report.contains("25 added"));
    assert!(report.contains("24 pending"));
    assert_eq!(watch(&state).checkpoint, baseline.checkpoint);
    assert_eq!(
        server.fixture.lock().unwrap().summary_batches,
        [(101..=148).collect::<Vec<_>>(), vec![149]]
    );
}

#[tokio::test]
async fn native_pixiv_watch_recovers_when_an_artwork_disappears_mid_scan() {
    let mut fixture = Fixture::default();
    fixture.artworks.insert(91, Artwork::from_time(date(10)));
    fixture.artworks.insert(92, Artwork::from_time(date(0)));
    let server = TestServer::start(fixture).await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    create_watch(&state, 1);
    run_watch(&state).await.unwrap();
    let archived = stored_id(&state, "91").unwrap();

    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.artworks.clear();
        fixture.artworks.insert(93, Artwork::from_time(date(30)));
        fixture.artworks.insert(94, Artwork::from_time(date(-10)));
        fixture.remove_during_summary = Some(93);
        fixture.summary_batches.clear();
    }
    run_watch(&state).await.unwrap();
    assert_eq!(
        server.fixture.lock().unwrap().summary_batches,
        [vec![93, 94], vec![94]]
    );
    assert_eq!(stored_id(&state, "91"), Some(archived));
    assert!(stored_id(&state, "93").is_none());
    assert!(stored_id(&state, "94").is_some());
    assert_eq!(watch(&state).checkpoint, Some(date(10)));

    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.artworks.insert(92, Artwork::from_time(date(0)));
        fixture.artworks.insert(93, Artwork::from_time(date(30)));
    }
    run_watch(&state).await.unwrap();
    assert_eq!(server.fixture.lock().unwrap().summary_batches[2], [93]);
    assert!(stored_id(&state, "92").is_none());
    assert!(stored_id(&state, "93").is_some());
}

#[tokio::test]
async fn pixiv_defers_only_after_two_complete_omissions_and_resumes_on_return() {
    let mut fixture = Fixture::default();
    fixture.artworks.insert(71, Artwork::from_time(date(0)));
    fixture.fail_downloads.insert((71, 0));
    let server = TestServer::start(fixture).await;
    let home = tempfile::tempdir().unwrap();
    let state = server.app(&home);
    create_watch(&state, 1);
    assert!(run_watch(&state).await.is_err());

    server.fixture.lock().unwrap().artworks.remove(&71);
    let first_omission = run_watch(&state).await.unwrap_err().to_string();
    assert!(first_omission.contains("1 pending"));
    for (status, malformed, expected) in [
        (Some(StatusCode::FORBIDDEN), false, "valid session"),
        (Some(StatusCode::TOO_MANY_REQUESTS), false, "rate limit"),
        (None, true, "incomplete creator listing"),
    ] {
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.index_error = status;
            fixture.incomplete_index = malformed;
        }
        let failure = run_watch(&state).await.unwrap_err().to_string();
        assert!(failure.contains(expected), "{failure}");
        assert!(failure.contains("1 pending"));
        assert!(failure.contains("post 71"));
        assert_eq!(watch(&state).checkpoint, Some(date(0)));
    }

    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.incomplete_index = false;
        fixture.requests.clear();
    }
    let report = run_watch(&state).await.unwrap().message();
    assert!(report.contains("0 pending · 1 deferred"));
    assert_eq!(server.fixture.lock().unwrap().requests.len(), 1);

    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.artworks.insert(71, Artwork::from_time(date(0)));
        fixture.fail_downloads.clear();
    }
    let report = run_watch(&state).await.unwrap().message();
    assert!(report.contains("1 added"));
    assert!(report.contains("0 pending"));
    assert!(stored_id(&state, "71").is_some());
    assert_eq!(server.fixture.lock().unwrap().summary_batches, [vec![71]]);
}

#[tokio::test]
async fn existing_pixiv_watches_keep_checkpoint_ties_and_can_switch_backends() {
    let mut fixture = Fixture::default();
    fixture.artworks.insert(81, Artwork::from_time(date(0)));
    fixture.artworks.insert(82, Artwork::from_time(date(10)));
    fixture.artworks.insert(83, Artwork::from_time(date(10)));
    fixture.fail_downloads.insert((83, 0));
    let server = TestServer::start(fixture).await;
    let home = tempfile::tempdir().unwrap();
    let mut state = server.app(&home);
    create_watch(&state, 1);
    state
        .db
        .with(|conn| {
            let watch = watches::get(conn, 1)?;
            watches::record_scan(conn, &watch, &[], date(10))
        })
        .unwrap();
    assert!(!watch(&state).pixiv_baselined);

    assert!(run_watch(&state).await.is_err());
    assert!(stored_id(&state, "81").is_none());
    assert!(stored_id(&state, "82").is_some());
    assert!(stored_id(&state, "83").is_none());
    server.fixture.lock().unwrap().artworks.remove(&83);
    assert!(run_watch(&state).await.is_err());
    assert!(
        run_watch(&state)
            .await
            .unwrap()
            .message()
            .contains("1 deferred")
    );

    write_gallery_dl(&home);
    std::fs::write(home.path().join("watch-list.json"), "[]").unwrap();
    Arc::make_mut(&mut state.cfg).pixiv_backend = PixivBackend::GalleryDl;
    let report = run_watch(&state).await.unwrap().message();
    assert!(report.contains("1 added"));
    assert!(stored_id(&state, "83").is_some());

    Arc::make_mut(&mut state.cfg).pixiv_backend = PixivBackend::Native;
    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.requests.clear();
        fixture.artworks.insert(83, Artwork::from_time(date(10)));
    }
    run_watch(&state).await.unwrap();
    assert_eq!(server.fixture.lock().unwrap().requests.len(), 1);
    assert!(stored_id(&state, "81").is_none());
    assert_eq!(watch(&state).checkpoint, Some(date(10)));
}
