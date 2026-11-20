//! Rate limited HTTP server and dashboard.
//!
//! ```text
//! cargo run -p server --bin web-server                         # in-process limiter
//! cargo run -p server --bin web-server -- --backend redis      # shared through Redis
//! cargo run -p server --bin web-server -- --concurrency gradient2  # shed overload with 503
//! cargo run -p server --bin web-server -- --llm                  # mock LLM with token limits
//! ```
//!
//! Every flag can also be set by environment variable (`DRL_BACKEND`, `DRL_LIMIT`, ...).

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use drl_core::concurrency::ConcurrencyLimiter;
use drl_core::{
    Algorithm, BreakerConfig, CircuitBreaker, FailurePolicy, MonotonicClock, RateLimitConfig,
};
use drl_redis_store::{GuardedLimiter, RedisOptions, RedisRateLimiter};
use server::backend::Backend;
use server::layer::Limits;
use server::strategy::StrategyBackend;
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
    /// A coordination strategy (see `--strategy`) with Redis as the shared store.
    Strategy,
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
    /// Coordination strategy for the strategy backend.
    #[arg(long, env = "DRL_STRATEGY", default_value = "adaptive_leasing")]
    strategy: String,
    /// Nodes in the cluster (strategies like local split need it).
    #[arg(long, env = "DRL_NODES", default_value_t = 3)]
    nodes: u64,
    /// Enable the benchmark-only /admin routes (strategy switching). Never in production.
    #[arg(long, env = "DRL_ADMIN", default_value_t = false)]
    admin: bool,
    /// Concurrency limit on the API routes: `off`, `fixed:N`, `aimd` or `gradient2`.
    /// Requests above it get `503` at once instead of queueing.
    #[arg(long, env = "DRL_CONCURRENCY", default_value = "off")]
    concurrency: String,
    /// Serve a mock LLM at POST /v1/completions behind a token limit (models `small`
    /// and `large`; `large` costs 3× per input token and 6× per output token).
    #[arg(long, env = "DRL_LLM", default_value_t = false)]
    llm: bool,
    /// How the token limit charges output: estimate_only, reserve_max or
    /// admit_then_reconcile.
    #[arg(
        long,
        env = "DRL_TOKEN_ACCOUNTING",
        default_value = "admit_then_reconcile"
    )]
    token_accounting: String,
    /// Input units per minute per caller.
    #[arg(long, env = "DRL_TOKEN_INPUT_PER_MIN", default_value_t = 100_000)]
    token_input_per_min: u64,
    /// Output units per minute per caller.
    #[arg(long, env = "DRL_TOKEN_OUTPUT_PER_MIN", default_value_t = 20_000)]
    token_output_per_min: u64,
    /// Directory with the built playground (`cd web && npm run build`). Static files are
    /// served from here when it exists.
    #[arg(long, env = "DRL_WEB_DIR", default_value = "web/dist")]
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
        BackendKind::Strategy => {
            let options = RedisOptions {
                response_timeout: Duration::from_millis(args.redis_timeout_ms),
                ..RedisOptions::default()
            };
            let store = connect_store(&args.redis_url, config, options).await?;
            let backend = StrategyBackend::start(
                &args.strategy,
                store,
                args.nodes,
                FailurePolicy::FailOpen,
                Duration::from_millis(args.redis_timeout_ms),
            )
            .map_err(anyhow::Error::msg)?;
            Backend::Strategy(backend)
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
    if !args.web_dir.is_dir() {
        tracing::info!(
            dir = %args.web_dir.display(),
            "no built playground found; serving the API only (build it with `just site`)"
        );
    }
    let concurrency = server::concurrency::parse(&args.concurrency).map_err(anyhow::Error::msg)?;
    if let Some(c) = &concurrency {
        tracing::info!(limit = c.name(), "concurrency limit on the API routes");
    }
    let mut router = app::router_with(
        Arc::clone(&limits),
        Some(prometheus),
        &args.web_dir,
        concurrency.map(|c| Arc::new(ConcurrencyLimiter::new(c))),
    );
    if args.llm {
        use drl_core::tokens::{Accounting, ModelPolicy, TokenLimiter, TokenPolicy};
        use server::llm::{MockModel, TokenLimitLayer, mock_router};
        let accounting: Accounting = args.token_accounting.parse()?;
        let policy = TokenPolicy::new(
            RateLimitConfig::per_minute(args.token_input_per_min)
                .context("invalid --token-input-per-min")?,
            RateLimitConfig::per_minute(args.token_output_per_min)
                .context("invalid --token-output-per-min")?,
            accounting,
        )
        .model("small", ModelPolicy::new(1, 1, 1_024, 256))
        .model("large", ModelPolicy::new(3, 6, 4_096, 512));
        let limiter = Arc::new(TokenLimiter::new(policy, MonotonicClock::new()));
        let mock = MockModel {
            first_token: Duration::from_millis(200),
            per_token: Duration::from_millis(20),
        };
        tracing::info!(
            accounting = accounting.name(),
            "mock LLM at POST /v1/completions"
        );
        router = router.merge(mock_router(mock).layer(TokenLimitLayer::new(limiter)));
    }
    if args.admin {
        tracing::warn!("benchmark-only /admin routes are enabled");
        router = router.merge(app::admin_router(limits));
    }
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
    let store = connect_store(&args.redis_url, config, options).await?;
    let breaker_config = BreakerConfig::new(
        args.breaker_failures,
        Duration::from_millis(args.breaker_open_ms),
    )?;
    Ok(GuardedLimiter::new(
        store,
        CircuitBreaker::new(breaker_config, MonotonicClock::new()),
    ))
}

/// Connects to Redis, retrying for a few seconds while it starts up.
async fn connect_store(
    url: &str,
    config: RateLimitConfig,
    options: RedisOptions,
) -> anyhow::Result<RedisRateLimiter> {
    let mut attempt = 0;
    loop {
        match RedisRateLimiter::connect_with(url, config, options.clone()).await {
            Ok(store) => return Ok(store),
            Err(e) if attempt < 20 => {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "Redis not reachable yet, retrying");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => return Err(e).context("connect to Redis"),
        }
    }
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
