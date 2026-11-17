//! Shared runtime state handed to the CLI, scheduler and web UI.

use std::sync::Arc;

use crate::blob::BlobStore;
use crate::config::Config;
use crate::db::Db;
use crate::error::Result;
use crate::render::Renderer;
use crate::tg::Transport;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub blobs: Arc<BlobStore>,
    pub renderer: Arc<Renderer>,
    pub transport: Option<Arc<dyn Transport>>,
}

impl AppState {
    pub fn open(cfg: Config, transport: Option<Arc<dyn Transport>>) -> Result<Self> {
        cfg.ensure_layout()?;
        let db = Db::open(cfg.db_path())?;
        let blobs = Arc::new(BlobStore::new(cfg.blobs_dir(), cfg.thumbs_dir())?);
        let renderer = Arc::new(Renderer::new()?);
        Ok(Self {
            cfg: Arc::new(cfg),
            db,
            blobs,
            renderer,
            transport,
        })
    }

    pub fn require_transport(&self) -> Result<Arc<dyn Transport>> {
        self.transport.clone().ok_or_else(|| {
            crate::error::Error::config("telegram transport unavailable (set TG_BOT_TOKEN)")
        })
    }
}
