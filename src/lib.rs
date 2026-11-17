//! nujijushi: a local artwork archive with a scheduled Telegram channel publisher.

pub mod blob;
pub mod bot;
pub mod cli;
pub mod config;
pub mod db;
pub mod error;
pub mod jobs;
pub mod media;
pub mod model;
pub mod nsfw;
pub mod queue;
pub mod render;
pub mod source;
pub mod state;
pub mod tg;
pub mod web;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
