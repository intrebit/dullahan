use crate::config::Config;
use crate::email::Mailer;
use crate::salt::SaltCache;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub mailer: Option<Mailer>,
    pub salt_cache: SaltCache,
}
