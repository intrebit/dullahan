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
    let dry_run = args.iter().any(|a| a == "--dry-run");
    if args.iter().any(|a| a == "--digest") {
        let pool = db::connect(&config.database_url).await?;
        let mailer = config.email.clone().map(Mailer::new);
        let now_ms = chrono::Utc::now().timestamp_millis();
        dullahan::digest::run(&pool, mailer.as_ref(), &config, now_ms, dry_run).await?;
        return Ok(());
    }

    // `dullahan --selfcheck` runs the operational checks once and exits. It
    // deliberately does *not* open the server's pool or apply migrations first:
    // "Postgres is unreachable" is one of the things it exists to report, so
    // failing to start on that would defeat the purpose. A non-zero exit on
    // findings is load-bearing too — it keeps systemd from recording success and
    // gives the external watchdog a second, independent signal.
    if args.iter().any(|a| a == "--selfcheck") {
        let findings = dullahan::selfcheck::run(&config, dry_run).await?;
        if !findings.is_empty() {
            std::process::exit(1);
        }
        return Ok(());
    }

    tracing::info!(addr = %config.bind_addr, "starting dullahan");

    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    // Load the tenant registry before serving. Failure here is fatal, exactly
    // like a failed migration — a process that is serving always has a loaded
    // snapshot, so there is no "not yet loaded" state to reason about.
    let sites = dullahan::sites::new_cache();
    let site_count = dullahan::sites::refresh(&pool, &sites).await?;

    // Decided once, here, and never recomputed from the live registry: if this
    // were derived per request from "is the cache empty", a transient DB failure
    // that emptied the cache would silently flip a locked-down deploy to
    // world-readable. See AppState::open_mode for why the registry size is not
    // part of the condition.
    let open_mode = config.admin_token.is_none();

    if config.admin_token.is_none() {
        tracing::warn!(
            "ADMIN_TOKEN is not set — /stats/* and all blog/product reads (including \
             drafts) are publicly readable, and every write endpoint is refused until \
             it is set. Set ADMIN_TOKEN to gate reads, enable authenticated writes, \
             and unlock the /sites tenant registry."
        );
    }
    if site_count == 0 {
        tracing::warn!(
            "the `sites` table is empty — every site id is admitted on /collect and \
             /stats/*. Register your tenants via POST /sites to gate them."
        );
    }

    {
        let pool = pool.clone();
        let sites = sites.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                // On failure the previous snapshot stays in place: a DB blip must
                // not un-gate ingest or lock out every tenant.
                if let Err(err) = dullahan::sites::refresh(&pool, &sites).await {
                    tracing::warn!(error = %err, "site registry refresh failed; keeping last snapshot");
                }
            }
        });
    }

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

    // Event retention. Unlike the salt prune above this is disk hygiene, not a
    // privacy guarantee, so a failure warns instead of being fatal — and it runs
    // only in this task, never inline, because the first sweep of a table that
    // has never been pruned can take a while and must not delay binding the port.
    if let Some(days) = config.retention_days {
        let pool = pool.clone();
        tokio::spawn(async move {
            // `interval` fires immediately, so the first sweep happens at startup.
            let mut tick = tokio::time::interval(Duration::from_secs(6 * 60 * 60));
            loop {
                tick.tick().await;
                let cutoff =
                    chrono::Utc::now().timestamp_millis() - (days as i64) * 24 * 60 * 60 * 1000;
                match dullahan::db::prune_events(&pool, cutoff).await {
                    Ok(0) => {}
                    Ok(removed) => {
                        tracing::info!(
                            removed,
                            retention_days = days,
                            "pruned old analytics events"
                        )
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to prune old analytics events")
                    }
                }
            }
        });
    }

    let mailer = config.email.clone().map(Mailer::new);

    let admin_token_hash = config
        .admin_token
        .as_deref()
        .map(dullahan::sites::token_digest);

    let (ingest_tx, ingest_writer) = dullahan::ingest::spawn_writer(pool.clone());

    let state = AppState {
        config: Arc::new(config.clone()),
        pool,
        mailer,
        salt_cache: dullahan::salt::new_cache(),
        ingest_tx,
        sites,
        open_mode,
        admin_token_hash,
    };

    let app = router_with_metrics(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // `serve` has returned, so every connection is drained and the router — the
    // last holder of an ingest sender — has been dropped. The writer therefore
    // sees a closed channel, flushes what is still queued, and exits. Awaiting it
    // is what makes a restart lossless instead of dropping the queue on the floor.
    if let Err(err) = ingest_writer.await {
        tracing::warn!(error = %err, "ingest writer did not shut down cleanly");
    }
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
