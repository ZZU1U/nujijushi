//! Keep Telegram's private command menus in sync with bot permissions.

use super::access::{Access, Role};
use super::commands;
use crate::db::users;
use crate::state::AppState;

pub(super) async fn sync_user(state: &AppState, chat_id: i64, role: Role) {
    if chat_id <= 0 {
        return;
    }
    let Some(transport) = state.transport.as_deref() else {
        return;
    };
    let access = Access::new(state.cfg.access.clone());
    let commands = commands::available(&access, role)
        .map(|command| (command.name, command.description))
        .collect();
    if transport
        .set_commands(Some(chat_id), commands)
        .await
        .is_err()
    {
        tracing::warn!(chat_id, "cannot update the Telegram command menu");
    }
}

pub(crate) async fn sync_stored_user(state: &AppState, tg_id: i64) {
    if state.transport.is_none() {
        return;
    }
    match state.db.call(move |conn| users::get(conn, tg_id)).await {
        Ok(Some(user)) => sync_user(state, tg_id, effective_role(state, tg_id, &user.role)).await,
        Ok(None) => {}
        Err(err) => tracing::warn!(
            tg_id,
            "cannot read the user for the Telegram command menu: {err}"
        ),
    }
}

/// Clear the legacy global menu and refresh existing private chats on startup.
pub(crate) async fn initialize(state: &AppState) {
    let Some(transport) = state.transport.as_deref() else {
        return;
    };
    if transport.set_commands(None, Vec::new()).await.is_err() {
        tracing::warn!("cannot clear the default Telegram command menu");
    }
    match state.db.call(|conn| users::list(conn, -1)).await {
        Ok(users) => {
            for user in users {
                sync_user(
                    state,
                    user.tg_id,
                    effective_role(state, user.tg_id, &user.role),
                )
                .await;
            }
        }
        Err(err) => tracing::warn!("cannot list users for the Telegram command menu: {err}"),
    }
}

fn effective_role(state: &AppState, tg_id: i64, stored: &str) -> Role {
    Role::parse(super::access::display_role(
        state.cfg.access.is_admin(tg_id),
        stored,
    ))
}
