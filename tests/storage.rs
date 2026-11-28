//! Storage sinks end to end: import -> mirror queue -> upload, plus proxy use.

use std::sync::Arc;

use nujijushi::config::Config;
use nujijushi::state::AppState;
use nujijushi::storage;
use nujijushi::tg::Transport;
use nujijushi::tg::mock::MockTransport;

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
}

fn harness(config: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    let transport: Arc<dyn Transport> = Arc::new(MockTransport::new());
    let state = AppState::open(cfg, Some(transport)).unwrap();
    Harness { _dir: dir, state }
}

/// A folder the local-dir importer can read: two images + a set sidecar.
fn fixture(dir: &std::path::Path) -> String {
    let set = dir.join("set-1");
    std::fs::create_dir_all(&set).unwrap();
    for name in ["a.png", "b.png"] {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([9, 9, 9]));
        img.save(set.join(name)).unwrap();
    }
    std::fs::write(
        set.join("post.json"),
        serde_json::json!({
            "id": "set-1",
            "artist": "Nightjar",
            "caption": "two panels",
            "tags": ["original"],
            "source": "pixiv",
            "source_url": "https://www.pixiv.net/artworks/1"
        })
        .to_string(),
    )
    .unwrap();
    set.to_str().unwrap().to_string()
}

fn config_with_storage(drive: &std::path::Path, on_import: bool) -> String {
    format!(
        r#"
[storage]
on_import = {on_import}
root = "nujijushi"

[storage.local]
enabled = true
path = "{}"
"#,
        drive.display()
    )
}

#[tokio::test]
async fn import_queues_and_uploads_to_a_local_folder() {
    let storage_dir = tempfile::tempdir().unwrap();
    let drive = storage_dir.path().join("drive");
    let harness = harness(&config_with_storage(&drive, true));
    let source = fixture(harness._dir.path());

    let outcome = nujijushi::jobs::import_target(&harness.state, &source, None)
        .await
        .unwrap();
    assert_eq!(outcome.post_ids.len(), 1);

    // queued by the import, uploaded by the worker
    let pending = harness
        .state
        .db
        .call(|conn| nujijushi::db::mirrors::pending(conn, None, 10))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "the import queued a mirror");
    assert_eq!(pending[0].sink, "local");

    let uploaded = storage::drain(&harness.state, 10).await.unwrap();
    assert_eq!(uploaded, 1);

    let post_dir = drive.join("nujijushi/pixiv/nightjar/set-1");
    assert!(post_dir.join("00.png").exists(), "media copied");
    assert!(post_dir.join("01.png").exists());
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(post_dir.join("post.json")).unwrap())
            .unwrap();
    assert_eq!(meta["source_id"], "set-1");
    assert_eq!(meta["tags"][0], "original");

    // and the queue is empty afterwards
    let pending = harness
        .state
        .db
        .call(|conn| nujijushi::db::mirrors::pending(conn, None, 10))
        .await
        .unwrap();
    assert!(pending.is_empty());
}

#[tokio::test]
async fn failures_are_recorded_and_retried() {
    let harness_dir = tempfile::tempdir().unwrap();
    // a regular file cannot become a directory: writes will fail
    let blocked = harness_dir.path().join("not-a-dir");
    std::fs::write(&blocked, b"x").unwrap();
    let config = config_with_storage(&blocked.join("nested"), true);
    let harness = harness(&config);
    let source = fixture(harness._dir.path());

    let outcome = nujijushi::jobs::import_target(&harness.state, &source, None)
        .await
        .unwrap();
    let uploaded = storage::drain(&harness.state, 10).await.unwrap();
    assert_eq!(uploaded, 0);

    let rows = harness
        .state
        .db
        .call({
            let post_id = outcome.post_ids[0];
            move |conn| nujijushi::db::mirrors::get(conn, post_id, "local")
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.state, "failed");
    assert!(rows.error.is_some(), "the reason is kept");
    assert_eq!(rows.attempts, 1);

    // a fresh failure waits for the retry delay, so a broken row cannot starve
    // the next drain batch
    let pending = harness
        .state
        .db
        .call(|conn| nujijushi::db::mirrors::pending(conn, None, 10))
        .await
        .unwrap();
    assert!(
        pending.is_empty(),
        "a fresh failure must not be retried immediately"
    );

    // once the backoff has passed it becomes eligible again
    harness
        .state
        .db
        .call({
            let post_id = outcome.post_ids[0];
            move |conn| {
                conn.execute(
                    "UPDATE mirrors SET updated_at = ?1 WHERE post_id = ?2 AND sink = 'local'",
                    rusqlite::params![
                        nujijushi::db::fmt_ts(
                            chrono::Utc::now()
                                - chrono::Duration::seconds(
                                    nujijushi::db::mirrors::FAILED_RETRY_SECS + 1
                                )
                        ),
                        post_id
                    ],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let pending = harness
        .state
        .db
        .call(|conn| nujijushi::db::mirrors::pending(conn, None, 10))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "the failure is retried after the delay");
}

#[tokio::test]
async fn sync_backfills_posts_imported_earlier() {
    let storage_dir = tempfile::tempdir().unwrap();
    let drive = storage_dir.path().join("drive");
    // on_import = false: nothing is queued at import time
    let harness = harness(&config_with_storage(&drive, false));
    let source = fixture(harness._dir.path());
    nujijushi::jobs::import_target(&harness.state, &source, None)
        .await
        .unwrap();

    let pending = harness
        .state
        .db
        .call(|conn| nujijushi::db::mirrors::pending(conn, None, 10))
        .await
        .unwrap();
    assert!(pending.is_empty());

    let summary = storage::sync(&harness.state, None, 50, false)
        .await
        .unwrap();
    assert!(summary.queued >= 1, "sync queued the missing mirror");
    assert!(summary.uploaded >= 1, "and uploaded it");
    assert!(drive.join("nujijushi/pixiv/nightjar/set-1/00.png").exists());
}

#[tokio::test]
async fn nothing_happens_without_a_sink() {
    let harness = harness("[telegram]\nbot_token = \"1:test\"\n");
    assert!(storage::sinks(&harness.state).is_empty());
    assert_eq!(storage::enqueue(&harness.state, &[1, 2]).await.unwrap(), 0);
    assert_eq!(storage::drain(&harness.state, 10).await.unwrap(), 0);
}

#[tokio::test]
async fn uploads_go_through_the_configured_proxy() {
    use axum::Router;
    use axum::http::StatusCode;

    // a fake HTTP proxy: records the absolute-form request and accepts it
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let app: Router = Router::new().fallback(move |uri: axum::http::Uri| {
        let recorder = recorder.clone();
        async move {
            recorder.lock().unwrap().push(uri.to_string());
            StatusCode::CREATED
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // the webdav base url cannot resolve: only the proxy can make this work
    let config = format!(
        r#"
[storage]
on_import = true

[storage.webdav]
enabled = true
url = "http://webdav.invalid/dav/art"
username = "me"
password = "secret"

[proxy]
url = "http://{proxy_addr}"
"#
    );
    let harness = harness(&config);
    let source = fixture(harness._dir.path());
    nujijushi::jobs::import_target(&harness.state, &source, None)
        .await
        .unwrap();

    let uploaded = storage::drain(&harness.state, 10).await.unwrap();
    assert_eq!(uploaded, 1, "the upload went through the proxy");

    let requests = seen.lock().unwrap().clone();
    assert!(!requests.is_empty(), "the proxy saw the requests");
    assert!(
        requests
            .iter()
            .any(|uri| uri.starts_with("http://webdav.invalid/dav/art/pixiv/nightjar/set-1/")),
        "absolute-form URIs reached the proxy: {requests:?}"
    );
}

#[tokio::test]
async fn a_partial_telegram_backup_is_blocked_instead_of_retried() {
    use nujijushi::db::posts::NewMedia;
    use nujijushi::source::NormalizedPost;
    use nujijushi::tg::TgError;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
[telegram]
bot_token = "1:test"

[storage.telegram]
enabled = true
chat_id = "-100123"
"#,
    )
    .unwrap();
    let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    // A photo and a video are two messages: the second one fails.
    let mock = Arc::new(MockTransport::with_script(vec![
        None,
        Some(TgError::Permanent("file is too big".into())),
    ]));
    let transport: Arc<dyn Transport> = mock.clone();
    let state = AppState::open(cfg, Some(transport)).unwrap();
    let post_id = state
        .db
        .call(|conn| {
            let post = NormalizedPost::new("pixiv", "backup-1");
            let media = vec![
                NewMedia {
                    blob_hash: "a".repeat(64),
                    ext: "png".into(),
                    mime: None,
                    width: Some(8),
                    height: Some(8),
                    bytes: 1,
                    is_video: false,
                    poster_hash: None,
                    sensitive: false,
                    meta_json: None,
                },
                NewMedia {
                    blob_hash: "b".repeat(64),
                    ext: "mp4".into(),
                    mime: None,
                    width: None,
                    height: None,
                    bytes: 1,
                    is_video: true,
                    poster_hash: None,
                    sensitive: false,
                    meta_json: None,
                },
            ];
            let (id, _) = nujijushi::db::posts::upsert(conn, &post, &media)?;
            Ok(id)
        })
        .await
        .unwrap();
    nujijushi::storage::enqueue(&state, &[post_id])
        .await
        .unwrap();
    assert_eq!(storage::drain(&state, 10).await.unwrap(), 0);

    let mirror = state
        .db
        .call(move |conn| nujijushi::db::mirrors::get(conn, post_id, "telegram"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mirror.state, "blocked");
    assert!(
        mirror.error.as_deref().unwrap().contains("partial"),
        "got {:?}",
        mirror.error
    );

    let calls = mock.send_calls();
    assert_eq!(storage::drain(&state, 10).await.unwrap(), 0);
    assert_eq!(
        mock.send_calls(),
        calls,
        "a partially uploaded backup is not retried automatically"
    );

    let saved = mirror.external_id.clone().expect("resume checkpoint");
    assert!(
        saved.contains("\"groups\":1"),
        "the first group is checkpointed: {saved}"
    );

    // After the operator verifies the channel, a retry resumes at group two.
    mock.append_script(vec![None]);
    state
        .db
        .call(move |conn| nujijushi::db::mirrors::enqueue(conn, post_id, "telegram"))
        .await
        .unwrap();
    assert_eq!(storage::drain(&state, 10).await.unwrap(), 1);
    let done = state
        .db
        .call(move |conn| nujijushi::db::mirrors::get(conn, post_id, "telegram"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(
        done.external_id.as_deref().unwrap().split(',').count(),
        2,
        "both groups are recorded"
    );
    assert_eq!(
        mock.send_calls(),
        calls + 1,
        "only the missing group was sent on retry"
    );
}
