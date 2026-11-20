//! Axum server for the rate limiter: a Tower [`layer::RateLimitLayer`] on real routes,
//! IETF `RateLimit` headers, a local or Redis [`backend::Backend`], and metrics.

#![forbid(unsafe_code)]

pub mod app;
pub mod backend;
pub mod headers;
pub mod layer;
pub mod metrics;
pub mod strategy;
