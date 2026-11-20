use actix_cors::Cors;
use actix_files as fs;
use actix_web::{App, HttpResponse, HttpServer, Result, get, post, web};
use serde::{Deserialize, Serialize};
use std::time::Instant;

use drl_core::{Algorithm, Cost, MonotonicClock, RateLimitConfig};
use server::metrics;

#[derive(Serialize)]
struct MetricsResponse {
    total: u64,
    allowed: u64,
    blocked: u64,
    allow_rate: f64,
}

#[derive(Deserialize)]
struct TestRequest {
    algorithm: String,
    max_requests: u64,
    window_seconds: u64,
    num_requests: u32,
}

#[derive(Serialize)]
struct TestResponse {
    allowed: u32,
    blocked: u32,
    duration_ms: f64,
    requests_per_sec: f64,
    results: Vec<bool>,
}

#[get("/")]
async fn index() -> Result<fs::NamedFile> {
    Ok(fs::NamedFile::open("web/index.html")?)
}

#[get("/api/metrics")]
async fn get_metrics() -> Result<HttpResponse> {
    let total = metrics::REQUESTS_TOTAL.get();
    let allowed = metrics::REQUESTS_ALLOWED.get();
    let blocked = metrics::REQUESTS_BLOCKED.get();

    let allow_rate = if total > 0 {
        (allowed as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    Ok(HttpResponse::Ok().json(MetricsResponse {
        total,
        allowed,
        blocked,
        allow_rate,
    }))
}

#[post("/api/test")]
async fn test_rate_limiter(req: web::Json<TestRequest>) -> Result<HttpResponse> {
    let algorithm: Algorithm = match req.algorithm.parse() {
        Ok(a) => a,
        Err(e) => return Ok(HttpResponse::BadRequest().body(e.to_string())),
    };
    let config = match RateLimitConfig::new(
        req.max_requests,
        std::time::Duration::from_secs(req.window_seconds),
    ) {
        Ok(c) => c,
        Err(e) => return Ok(HttpResponse::BadRequest().body(e.to_string())),
    };
    let limiter = algorithm.build(config, MonotonicClock::new());

    let start = Instant::now();
    let mut results = Vec::with_capacity(req.num_requests as usize);
    for _ in 0..req.num_requests {
        let req_start = Instant::now();
        let allowed = limiter.check("test_user", Cost::ONE).allowed;
        metrics::record_request(allowed, req_start);
        results.push(allowed);
    }

    let duration = start.elapsed();
    let allowed = results.iter().filter(|&&a| a).count() as u32;
    Ok(HttpResponse::Ok().json(TestResponse {
        allowed,
        blocked: req.num_requests - allowed,
        duration_ms: duration.as_secs_f64() * 1000.0,
        requests_per_sec: req.num_requests as f64 / duration.as_secs_f64(),
        results,
    }))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize metrics
    metrics::init_metrics();

    println!("🚀 Starting Rate Limiter Web Dashboard");
    println!("📊 Dashboard: http://localhost:3001");
    println!("📈 Metrics API: http://localhost:3001/api/metrics");
    println!("\nPress Ctrl+C to stop\n");

    HttpServer::new(|| {
        let cors = Cors::permissive();

        App::new()
            .wrap(cors)
            .service(index)
            .service(get_metrics)
            .service(test_rate_limiter)
            .service(fs::Files::new("/", "web").index_file("index.html"))
    })
    .bind(("127.0.0.1", 3001))?
    .run()
    .await
}
