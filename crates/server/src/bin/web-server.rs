//! Rate limited HTTP server and dashboard.
//!
//! ```text
//! cargo run -p server --bin web-server                         # in-process limiter
//! cargo run -p server --bin web-server -- --backend redis      # shared through Redis
//! ```
//!
//! Every flag can also be set by environment variable (`DRL_BACKEND`, `DRL_LIMIT`, ...).

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use drl_core::{Algorithm, BreakerConfig, CircuitBreaker, MonotonicClock, RateLimitConfig};
use drl_redis_store::{GuardedLimiter, RedisOptions, RedisRateLimiter};
use server::backend::Backend;
use server::layer::Limits;
use server::{app, metrics};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendKind {
    /// In-process state (one node).
    Local,
    /// State in Redis, shared by every node.
    Redis,
}

#[derive(Debug, Parser)]
#[command(about = "Rate limited HTTP server with IETF RateLimit headers")]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "DRL_BIND", default_value = "127.0.0.1:3001")]
    bind: SocketAddr,
    /// Name sent in the x-drl-node header.
    #[arg(long, env = "DRL_NODE_ID", default_value = "local")]
    node_id: String,
    /// Where limiter state lives.
    #[arg(long, value_enum, env = "DRL_BACKEND", default_value = "local")]
    backend: BackendKind,
    /// Algorithm for the local backend (the Redis backend is a token bucket).
    #[arg(long, env = "DRL_ALGORITHM", default_value = "token_bucket")]
    algorithm: String,
    /// Units allowed per window, per caller and route.
    #[arg(long, env = "DRL_LIMIT", default_value_t = 100)]
    limit: u64,
    /// Window length in milliseconds.
    #[arg(long, env = "DRL_WINDOW_MS", default_value_t = 1000)]
    window_ms: u64,
    /// Redis URL for the Redis backend.
    #[arg(long, env = "DRL_REDIS_URL", default_value = "redis://127.0.0.1:6379/")]
    redis_url: String,
    /// Redis reply timeout in milliseconds; a timeout counts as a breaker failure.
    #[arg(long, env = "DRL_REDIS_TIMEOUT_MS", default_value_t = 100)]
    redis_timeout_ms: u64,
    /// Consecutive Redis failures that open the circuit breaker.
    #[arg(long, env = "DRL_BREAKER_FAILURES", default_value_t = 5)]
    breaker_failures: u32,
    /// How long the breaker stays open, in milliseconds.
    #[arg(long, env = "DRL_BREAKER_OPEN_MS", default_value_t = 1000)]
    breaker_open_ms: u64,
    /// Directory with the dashboard's static files.
    #[arg(long, env = "DRL_WEB_DIR", default_value = "web")]
    web_dir: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    let prometheus = metrics::install()?;

    let config = RateLimitConfig::new(args.limit, Duration::from_millis(args.window_ms))
        .context("invalid --limit / --window-ms")?;
    let algorithm: Algorithm = args.algorithm.parse()?;
    let backend = match args.backend {
        BackendKind::Local => Backend::Local {
            limiter: algorithm.build(config, MonotonicClock::new()),
            config,
        },
        BackendKind::Redis => {
            if algorithm != Algorithm::TokenBucket {
                bail!("the Redis backend implements token_bucket only");
            }
            Backend::Redis(connect_redis(&args, config).await?)
        }
    };
    tracing::info!(
        node = %args.node_id,
        backend = backend.name(),
        algorithm = algorithm.name(),
        limit = config.limit(),
        window_ms = args.window_ms,
        "starting"
    );

    let limits = Arc::new(Limits {
        backend,
        stats: Default::default(),
        node_id: args.node_id,
    });
    let router = app::router(limits, Some(prometheus), &args.web_dir);
    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("bind {}", args.bind))?;
    tracing::info!("listening on http://{}", args.bind);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}

/// Connects to Redis, retrying for a few seconds while it starts up.
async fn connect_redis(
    args: &Args,
    config: RateLimitConfig,
) -> anyhow::Result<GuardedLimiter<MonotonicClock>> {
    let options = RedisOptions {
        response_timeout: Duration::from_millis(args.redis_timeout_ms),
        ..RedisOptions::default()
    };
    let mut attempt = 0;
    let store = loop {
        match RedisRateLimiter::connect_with(&args.redis_url, config, options.clone()).await {
            Ok(store) => break store,
            Err(e) if attempt < 20 => {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "Redis not reachable yet, retrying");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => return Err(e).context("connect to Redis"),
        }
    };
    let breaker_config = BreakerConfig::new(
        args.breaker_failures,
        Duration::from_millis(args.breaker_open_ms),
    )?;
    Ok(GuardedLimiter::new(
        store,
        CircuitBreaker::new(breaker_config, MonotonicClock::new()),
    ))
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
