//! Video poster frames, generated with ffmpeg (optional dependency).

use std::path::Path;

use crate::config::Config;
use crate::error::Result;
use crate::source::gallerydl::run_with_timeout;

/// Extract a single frame as JPEG. Returns false when ffmpeg is unavailable or
/// the video cannot be read - publishing then simply proceeds without a poster.
pub async fn extract_poster(cfg: &Config, video: &Path, out: &Path) -> Result<bool> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let args: Vec<String> = vec![
        "-y".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        "0.5".into(),
        "-i".into(),
        video.display().to_string(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale=512:-1".into(),
        "-q:v".into(),
        "4".into(),
        out.display().to_string(),
    ];
    match run_with_timeout(&cfg.ffmpeg, &args, 60).await {
        Ok(output) if output.status.success() && out.exists() => Ok(true),
        Ok(_) => {
            tracing::debug!(video = %video.display(), "ffmpeg could not extract a poster");
            Ok(false)
        }
        Err(err) => {
            tracing::debug!("ffmpeg unavailable for poster extraction: {err}");
            Ok(false)
        }
    }
}
