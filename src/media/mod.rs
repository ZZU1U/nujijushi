//! Media planning: which files go into which Telegram message.

pub mod compress;
pub mod grouping;
pub mod merge;
pub mod video;

pub use grouping::{MessageGroup, MessageShape, plan_messages};
