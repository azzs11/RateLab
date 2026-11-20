//! Runs the example API on http://127.0.0.1:3000.
//!
//! ```text
//! cargo run -p axum-api-example
//! curl -i -H 'x-api-key: alice' localhost:3000/hello
//! ```

use axum_api_example::{Limits, app};
use ratelab::concurrency::{Concurrency, ConcurrencyLimiter, Gradient2Config};
use ratelab::{Gcra, RateLimitConfig};
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 5 requests per second per API key, and as many in flight as Gradient2 judges safe.
    let config = RateLimitConfig::per_second(5)?;
    let limits = Limits {
        rate: Arc::new(Gcra::new(config)),
        config,
        concurrency: Arc::new(ConcurrencyLimiter::new(Concurrency::Gradient2(
            Gradient2Config::default(),
        ))),
        origin: Instant::now(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!(
        "listening on http://127.0.0.1:3000 (try: curl -i -H 'x-api-key: alice' localhost:3000/hello)"
    );
    axum::serve(listener, app(limits)).await?;
    Ok(())
}
