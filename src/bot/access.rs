//! Who may do what: config admins/guests, database roles and daily quotas.

use crate::config::AccessConfig;
use crate::db::{usage, users};
use crate::error::Result;
use crate::state::AppState;
use crate::tg::botapi::BotUser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Guest,
    /// Unknown or explicitly banned.
    Banned,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Guest => "guest",
            Role::Banned => "guest (not allowed)",
        }
    }

    pub fn parse(raw: &str) -> Role {
        match raw.trim().to_ascii_lowercase().as_str() {
            "admin" => Role::Admin,
            "banned" | "ban" => Role::Banned,
            _ => Role::Guest,
        }
    }

    pub fn is_admin(self) -> bool {
        self == Role::Admin
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perm {
    Import,
    Browse,
    Queue,
    Publish,
    Curate,
}

impl Perm {
    pub fn as_str(self) -> &'static str {
        match self {
            Perm::Import => "import",
            Perm::Browse => "browse",
            Perm::Queue => "queue",
            Perm::Publish => "publish",
            Perm::Curate => "curate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaKind {
    Import,
    Queue,
    Publish,
}

impl QuotaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaKind::Import => "import",
            QuotaKind::Queue => "queue",
            QuotaKind::Publish => "publish",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Access {
    cfg: AccessConfig,
}

impl Access {
    pub fn new(cfg: AccessConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &AccessConfig {
        &self.cfg
    }

    pub fn admins(&self) -> &[i64] {
        &self.cfg.admins
    }

    /// Publishing is opted into separately and never available to guests.
    pub fn allows(&self, role: Role, perm: Perm) -> bool {
        match role {
            Role::Banned => false,
            Role::Admin => match perm {
                Perm::Publish => self.cfg.allow_publish,
                _ => true,
            },
            Role::Guest => match perm {
                Perm::Import => self.cfg.allow_import,
                Perm::Browse => self.cfg.allow_browse,
                Perm::Queue => self.cfg.allow_queue,
                Perm::Publish => false,
                Perm::Curate => self.cfg.allow_curate,
            },
        }
    }

    pub fn allows_nsfw_browse(&self, role: Role) -> bool {
        match role {
            Role::Admin => true,
            _ => self.cfg.allow_nsfw_browse,
        }
    }

    /// Config admins, then database role, then the configured guest list, then `public`.
    pub async fn resolve(&self, state: &AppState, user: &BotUser) -> Result<Role> {
        if self.cfg.is_admin(user.id) {
            self.remember(state, user).await?;
            return Ok(Role::Admin);
        }
        let existing = {
            let id = user.id;
            state.db.call(move |conn| users::get(conn, id)).await?
        };
        if let Some(stored) = &existing {
            match Role::parse(&stored.role) {
                Role::Banned => return Ok(Role::Banned),
                Role::Admin => {
                    self.remember(state, user).await?;
                    return Ok(Role::Admin);
                }
                Role::Guest => {
                    self.remember(state, user).await?;
                    return Ok(Role::Guest);
                }
            }
        }
        if self.cfg.is_listed_guest(user.id) || self.cfg.public {
            self.remember(state, user).await?;
            return Ok(Role::Guest);
        }
        Ok(Role::Banned)
    }

    async fn remember(&self, state: &AppState, user: &BotUser) -> Result<()> {
        let (id, username, first_name) = (user.id, user.username.clone(), user.first_name.clone());
        state
            .db
            .call(move |conn| users::touch(conn, id, username.as_deref(), first_name.as_deref()))
            .await
    }

    /// Count one action against today's quota. Admins are exempt.
    pub async fn spend(
        &self,
        state: &AppState,
        role: Role,
        tg_id: i64,
        kind: QuotaKind,
    ) -> Result<bool> {
        if role.is_admin() {
            return Ok(true);
        }
        let limit = match kind {
            QuotaKind::Import => self.cfg.import_quota_per_day,
            QuotaKind::Queue => self.cfg.queue_quota_per_day,
            QuotaKind::Publish => 0,
        };
        state
            .db
            .call(move |conn| usage::bump(conn, tg_id, kind.as_str(), limit))
            .await
    }

    pub async fn used_today(&self, state: &AppState, tg_id: i64, kind: QuotaKind) -> Result<u32> {
        let kind = kind.as_str().to_string();
        state
            .db
            .call(move |conn| usage::count(conn, tg_id, &kind))
            .await
    }

    /// Hand a quota unit back when the paid action could not be started.
    pub async fn refund(&self, state: &AppState, tg_id: i64, kind: QuotaKind) -> Result<()> {
        let kind = kind.as_str().to_string();
        state
            .db
            .call(move |conn| usage::refund(conn, tg_id, &kind))
            .await
    }
}

/// The role a listing must show: config admins are admins, whatever the `users`
/// row says (messaging the bot auto-registers a row as `guest`).
///
/// The CLI, the bot's `/users` and the web page all read this one function, so a
/// listing can never contradict what `resolve` will actually allow.
pub fn display_role(config_admin: bool, stored: &str) -> &str {
    if config_admin { "admin" } else { stored.trim() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tg::Transport;
    use crate::tg::mock::MockTransport;
    use std::sync::Arc;

    fn access() -> Access {
        Access::new(AccessConfig {
            admins: vec![1],
            guests: vec![2],
            public: false,
            ..Default::default()
        })
    }

    fn user(id: i64) -> BotUser {
        BotUser {
            id,
            username: Some(format!("u{id}")),
            first_name: None,
            is_bot: false,
        }
    }

    async fn state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        let transport: Arc<dyn Transport> = Arc::new(MockTransport::new());
        std::mem::forget(dir);
        AppState::open(cfg, Some(transport)).unwrap()
    }

    #[tokio::test]
    async fn config_admin_wins_and_is_remembered() {
        let state = state().await;
        let access = access();
        assert_eq!(access.resolve(&state, &user(1)).await.unwrap(), Role::Admin);
        let stored = state
            .db
            .call(|conn| crate::db::users::get(conn, 1))
            .await
            .unwrap();
        assert!(stored.is_some(), "admins are recorded on sight");
    }

    #[tokio::test]
    async fn database_roles_are_honoured() {
        let state = state().await;
        let access = access();
        state
            .db
            .call(|conn| crate::db::users::set_role(conn, 5, "banned"))
            .await
            .unwrap();
        assert_eq!(
            access.resolve(&state, &user(5)).await.unwrap(),
            Role::Banned
        );

        state
            .db
            .call(|conn| crate::db::users::set_role(conn, 6, "admin"))
            .await
            .unwrap();
        assert_eq!(access.resolve(&state, &user(6)).await.unwrap(), Role::Admin);

        state
            .db
            .call(|conn| crate::db::users::set_role(conn, 7, "guest"))
            .await
            .unwrap();
        assert_eq!(access.resolve(&state, &user(7)).await.unwrap(), Role::Guest);
    }

    #[tokio::test]
    async fn unknown_users_depend_on_public() {
        let state = state().await;
        let closed = access();
        assert_eq!(
            closed.resolve(&state, &user(9)).await.unwrap(),
            Role::Banned,
            "private bot refuses strangers"
        );

        let open = Access::new(AccessConfig {
            public: true,
            ..Default::default()
        });
        assert_eq!(open.resolve(&state, &user(9)).await.unwrap(), Role::Guest);
        let stored = state
            .db
            .call(|conn| crate::db::users::get(conn, 9))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.role, "guest", "auto-registered on first contact");
    }

    #[test]
    fn permission_matrix() {
        let access = access();
        // guest defaults: import only
        assert!(access.allows(Role::Guest, Perm::Import));
        assert!(!access.allows(Role::Guest, Perm::Browse));
        assert!(!access.allows(Role::Guest, Perm::Queue));
        assert!(!access.allows(Role::Guest, Perm::Publish));
        assert!(!access.allows(Role::Guest, Perm::Curate));
        // admins do everything except publishing, which is opt-in
        assert!(access.allows(Role::Admin, Perm::Import));
        assert!(access.allows(Role::Admin, Perm::Queue));
        assert!(!access.allows(Role::Admin, Perm::Publish));
        // banned users get nothing
        assert!(!access.allows(Role::Banned, Perm::Import));
    }

    #[test]
    fn publish_stays_admin_only_even_when_enabled() {
        let access = Access::new(AccessConfig {
            allow_publish: true,
            allow_import: true,
            ..Default::default()
        });
        assert!(access.allows(Role::Admin, Perm::Publish));
        assert!(!access.allows(Role::Guest, Perm::Publish));
    }

    /// A config admin carries a `guest` row from their first message; every
    /// listing must show the role the bot will actually use.
    #[test]
    fn listings_show_the_effective_role() {
        assert_eq!(display_role(true, "guest"), "admin");
        assert_eq!(display_role(true, "banned"), "admin");
        assert_eq!(display_role(true, "admin"), "admin");
        assert_eq!(display_role(false, "guest"), "guest");
        assert_eq!(display_role(false, "banned"), "banned");
        assert_eq!(display_role(false, " admin "), "admin");
    }

    #[tokio::test]
    async fn quotas_apply_to_guests_only() {
        let state = state().await;
        let access = Access::new(AccessConfig {
            import_quota_per_day: 1,
            ..Default::default()
        });

        assert!(
            access
                .spend(&state, Role::Guest, 11, QuotaKind::Import)
                .await
                .unwrap()
        );
        assert!(
            !access
                .spend(&state, Role::Guest, 11, QuotaKind::Import)
                .await
                .unwrap()
        );
        assert_eq!(
            access
                .used_today(&state, 11, QuotaKind::Import)
                .await
                .unwrap(),
            1
        );
        // admins are exempt
        for _ in 0..5 {
            assert!(
                access
                    .spend(&state, Role::Admin, 11, QuotaKind::Import)
                    .await
                    .unwrap()
            );
        }
    }
}
