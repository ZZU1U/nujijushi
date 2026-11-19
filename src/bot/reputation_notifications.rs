//! Send committed reputation changes without making curation wait on Telegram.

use std::future::Future;
use std::time::Duration;

use crate::db::reputation;
use crate::error::Result;
use crate::state::AppState;

use super::reply;

pub async fn monitor_while<T>(state: &AppState, command: impl Future<Output = T>) -> T {
    if !state.cfg.guest_reputation || state.transport.is_none() {
        return command.await;
    }
    let monitor = async {
        loop {
            check(state).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    };
    tokio::select! {
        result = command => result,
        result = monitor => result,
    }
}

pub async fn check(state: &AppState) {
    if state.cfg.guest_reputation
        && let Err(err) = deliver(state).await
    {
        tracing::warn!("cannot read reputation notifications: {err}");
    }
}

async fn deliver(state: &AppState) -> Result<()> {
    let Some(transport) = state.transport.as_deref() else {
        return Ok(());
    };
    let cfg = state.cfg.clone();
    for _ in 0..16 {
        let cfg = cfg.clone();
        let next = state
            .db
            .call(move |conn| {
                let Some(event) = reputation::claim_notification(conn)? else {
                    return Ok(None);
                };
                let guest = reputation::is_guest(conn, &cfg.access, event.user_id)?;
                Ok(Some((event, guest)))
            })
            .await?;
        let Some((event, guest)) = next else {
            break;
        };
        if !guest {
            continue;
        }
        let sign = if event.delta > 0 { "+" } else { "" };
        let mut html = format!(
            "{}\nReputation {sign}{} · balance {}",
            reply::esc(&event.reason),
            reputation::format_points(event.delta),
            reputation::format_points(event.balance)
        );
        if let Some(url) = event.url {
            html.push_str(&format!("\n{}", reply::esc(&url)));
        }
        // A timeout can follow accepted delivery. Claim once to avoid sending it twice.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            reply::send(transport, &event.user_id.to_string(), &html),
        )
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(
                event = event.id,
                user = event.user_id,
                "reputation notification failed"
            );
        }
    }
    Ok(())
}
