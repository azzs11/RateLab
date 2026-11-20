//! A backend with limited capacity, for overload benchmarks and demos.
//!
//! [`CapacityBackend`] has a number of workers (changeable while running) and a FIFO
//! queue; each job holds one worker for its service time. [`work_router`] serves
//! `GET /work`: the job's service time comes from the `x-work-us` header (so a load
//! generator can replay exactly the jobs a simulation used), and the server answers
//! `504` if the job is not done by the deadline. Like many real backends, it does not
//! cancel the job: the work is still done later, for nobody.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;

/// Header with the job's service time in microseconds.
pub const WORK_HEADER: &str = "x-work-us";

#[derive(Debug, Default)]
struct Gate {
    workers: usize,
    busy: usize,
    queue: VecDeque<oneshot::Sender<()>>,
}

/// Workers and a FIFO queue.
#[derive(Debug)]
pub struct CapacityBackend {
    gate: Mutex<Gate>,
}

impl CapacityBackend {
    /// A backend with `workers` workers.
    pub fn new(workers: usize) -> Arc<Self> {
        Arc::new(Self {
            gate: Mutex::new(Gate {
                workers,
                ..Gate::default()
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the number of workers. Running jobs finish; queued ones start when a
    /// worker is free.
    pub fn set_workers(&self, workers: usize) {
        let mut g = self.lock();
        g.workers = workers;
        Self::hand_over(&mut g);
    }

    /// `(busy workers, queued jobs)`.
    pub fn load(&self) -> (usize, usize) {
        let g = self.lock();
        (g.busy, g.queue.len())
    }

    /// Starts queued jobs while workers are free.
    fn hand_over(g: &mut Gate) {
        while g.busy < g.workers {
            let Some(next) = g.queue.pop_front() else {
                return;
            };
            if next.send(()).is_ok() {
                g.busy += 1;
            }
        }
    }

    /// Waits for a worker, holds it for `service`, then frees it.
    pub async fn work(&self, service: Duration) {
        let wait = {
            let mut g = self.lock();
            if g.busy < g.workers && g.queue.is_empty() {
                g.busy += 1;
                None
            } else {
                let (tx, rx) = oneshot::channel();
                g.queue.push_back(tx);
                Some(rx)
            }
        };
        if let Some(rx) = wait {
            // The sender is only dropped after a worker was handed over to us.
            let _ = rx.await;
        }
        tokio::time::sleep(service).await;
        let mut g = self.lock();
        g.busy = g.busy.saturating_sub(1);
        Self::hand_over(&mut g);
    }
}

#[derive(Clone)]
struct WorkState {
    backend: Arc<CapacityBackend>,
    deadline: Duration,
}

/// `GET /work` on `backend`, answering `504` after `deadline`.
pub fn work_router(backend: Arc<CapacityBackend>, deadline: Duration) -> Router {
    Router::new()
        .route("/work", get(work))
        .with_state(WorkState { backend, deadline })
}

async fn work(State(state): State<WorkState>, headers: HeaderMap) -> (StatusCode, &'static str) {
    let Some(us) = headers
        .get(WORK_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return (StatusCode::BAD_REQUEST, "missing x-work-us");
    };
    let backend = Arc::clone(&state.backend);
    // Detached: the job runs to the end even if the answer has already gone out.
    let job = tokio::spawn(async move { backend.work(Duration::from_micros(us)).await });
    match tokio::time::timeout(state.deadline, job).await {
        Ok(Ok(())) => (StatusCode::OK, "done"),
        Ok(Err(_)) => (StatusCode::INTERNAL_SERVER_ERROR, "job failed"),
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, "deadline"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn job(us: u64) -> Request<Body> {
        Request::builder()
            .uri("/work")
            .header(WORK_HEADER, us)
            .body(Body::empty())
            .unwrap()
    }

    // Tokio's paused clock: timers advance instantly, nothing really sleeps.
    #[tokio::test(start_paused = true)]
    async fn answers_within_the_deadline_or_504_but_still_does_the_work() {
        let backend = CapacityBackend::new(1);
        let app = work_router(Arc::clone(&backend), Duration::from_millis(100));
        let start = tokio::time::Instant::now();
        let a = tokio::spawn(app.clone().oneshot(job(60_000)));
        let b = tokio::spawn(app.clone().oneshot(job(60_000)));
        assert_eq!(a.await.unwrap().unwrap().status(), StatusCode::OK);
        // b waits 60 ms for the worker, then needs 60 ms more: past the deadline.
        assert_eq!(
            b.await.unwrap().unwrap().status(),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(start.elapsed(), Duration::from_millis(100));
        assert_eq!(backend.load(), (1, 0), "the late job is still running");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(backend.load(), (0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn more_workers_start_queued_jobs() {
        let backend = CapacityBackend::new(0);
        let b = Arc::clone(&backend);
        let job = tokio::spawn(async move { b.work(Duration::from_millis(5)).await });
        tokio::task::yield_now().await;
        assert_eq!(backend.load(), (0, 1));
        backend.set_workers(1);
        assert_eq!(backend.load(), (1, 0));
        job.await.unwrap();
        assert_eq!(backend.load(), (0, 0));
    }

    #[tokio::test]
    async fn a_missing_service_time_is_a_bad_request() {
        let app = work_router(CapacityBackend::new(1), Duration::from_millis(100));
        let r = app
            .oneshot(Request::builder().uri("/work").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
