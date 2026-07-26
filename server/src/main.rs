use dullahan::{config::Config, db, email::Mailer, router_with_metrics, state::AppState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into());
    let json_logs = std::env::var("LOG_FORMAT")
        .map(|s| s.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    let config = Config::from_env()?;

    // `dullahan --digest` runs the weekly digest once and exits (driven by a
    // systemd timer). `--dry-run` prints each email instead of sending it.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--digest") {
        let dry_run = args.iter().any(|a| a == "--dry-run");
        let pool = db::connect(&config.database_url).await?;
        let mailer = config.email.clone().map(Mailer::new);
        let now_ms = chrono::Utc::now().timestamp_millis();
        dullahan::digest::run(&pool, mailer.as_ref(), &config, now_ms, dry_run).await?;
        return Ok(());
    }

    tracing::info!(addr = %config.bind_addr, "starting dullahan");

    if config.admin_token.is_none() {
        tracing::warn!(
            "ADMIN_TOKEN is not set — /stats/* and all blog reads (including drafts) \
             are publicly readable, and the blog write endpoints (POST/PATCH/DELETE \
             /posts) are refused until it is set. Set ADMIN_TOKEN to gate reads and \
             enable authenticated writes."
        );
    }

    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;
    dullahan::salt::prune_old_salts(&pool, chrono::Utc::now().date_naive()).await?;
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                tick.tick().await;
                if let Err(err) =
                    dullahan::salt::prune_old_salts(&pool, chrono::Utc::now().date_naive()).await
                {
                    tracing::warn!(error = %err, "failed to prune old daily salts");
                }
            }
        });
    }

    let mailer = config.email.clone().map(Mailer::new);

    let state = AppState {
        config: Arc::new(config.clone()),
        pool,
        mailer,
        salt_cache: dullahan::salt::new_cache(),
    };

    let app = router_with_metrics(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining connections");
}
