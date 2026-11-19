//! Optional Telegram DMs for operators who are away from the admin dashboard.

use std::future::Future;
use std::time::Duration;

use crate::db::queue_alerts;
use crate::error::Result;
use crate::state::AppState;

use super::reply;

const CHECK_INTERVAL: Duration = Duration::from_secs(15);
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub fn recipients(admins: &[i64]) -> Vec<i64> {
    let mut recipients: Vec<i64> = admins.iter().copied().filter(|&id| id > 0).collect();
    recipients.sort_unstable();
    recipients.dedup();
    recipients
}

/// Drive alerts independently of publishing and HTTP request handling.
pub async fn monitor_while<T>(state: &AppState, command: impl Future<Output = T>) -> T {
    if !state.cfg.queue_alerts {
        return command.await;
    }
    tokio::select! {
        result = command => result,
        result = monitor(state) => match result {},
    }
}

async fn monitor(state: &AppState) -> std::convert::Infallible {
    loop {
        check(state).await;
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

pub async fn check(state: &AppState) {
    if state.cfg.queue_alerts
        && let Err(err) = deliver(state).await
    {
        tracing::warn!("cannot check Telegram queue alerts: {err}");
    }
}

async fn deliver(state: &AppState) -> Result<()> {
    let Some(transport) = state.transport.as_deref() else {
        return Ok(());
    };
    let admins = recipients(&state.cfg.access.admins);
    if admins.is_empty() {
        return Ok(());
    }
    for candidate in state.db.call(|conn| queue_alerts::pending(conn)).await? {
        let Some(alert) = state
            .db
            .call(move |conn| queue_alerts::claim(conn, candidate))
            .await?
        else {
            continue;
        };
        let alias = reply::esc(&alert.channel_alias);
        let html = match alert.unconfirmed {
            Some((queue_id, post_id)) => format!(
                "<b>Unconfirmed send</b> for <code>{alias}</code>\nQueue #{queue_id} · post #{post_id}\nReview it in the admin dashboard under Unconfirmed sends."
            ),
            None => format!("<b>Empty queue</b> for <code>{alias}</code>"),
        };
        for admin in &admins {
            let chat_id = admin.to_string();
            match tokio::time::timeout(SEND_TIMEOUT, reply::send(transport, &chat_id, &html)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(_)) => tracing::warn!(admin, ?candidate, "Telegram queue alert failed"),
                Err(_) => tracing::warn!(admin, ?candidate, "Telegram queue alert timed out"),
            }
        }
    }
    Ok(())
}
