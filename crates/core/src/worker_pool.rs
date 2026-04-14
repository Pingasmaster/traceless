//! Process-wide bounded worker pool for background file operations.
//!
//! Before this module existed, `FileStore::add_files` and
//! `FileStore::clean_files` spawned one OS thread per path. Dropping a
//! directory of a few thousand files onto the window exhausted
//! `RLIMIT_NPROC` and panicked the calling thread via
//! `thread::spawn`'s internal `expect`, crashing the frontend.
//!
//! This module replaces the per-file spawns with a static pool of
//! `min(available_parallelism(), MAX_WORKERS)` long-lived workers that
//! pull `Box<dyn FnOnce() + Send>` jobs off a shared unbounded
//! `async_channel`. The pool is lazily initialized on the first
//! `submit` call and lives for the entire process lifetime: its
//! workers never exit (the sender stays alive forever via the static
//! itself), so there is nothing to join or drop.
//!
//! Each job is wrapped in `catch_unwind`. A handler panic on one file
//! is logged and the worker continues with the next job instead of
//! silently disappearing from the pool.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;
use std::thread;

use async_channel::{Receiver, Sender};

/// Upper bound on pool size. I/O-bound handlers (ffmpeg, lofty) and
/// the cxx-qt main loop both do badly with dozens of concurrent
/// workers, and real traceless workloads are dominated by a handful
/// of large files rather than thousands of small ones. `mat2
/// --parallel` defaults to the CPU count with a similar cap and the
/// same rationale.
const MAX_WORKERS: usize = 8;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct WorkerPool {
    tx: Sender<Job>,
}

impl WorkerPool {
    fn new() -> Self {
        let (tx, rx) = async_channel::unbounded::<Job>();
        let worker_count = thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4)
            .min(MAX_WORKERS);
        for _ in 0..worker_count {
            let rx: Receiver<Job> = rx.clone();
            // `worker_loop` is spawned directly here rather than via
            // `FileStore::add_files`-style submit so the pool owns its
            // own workers. Any spawn failure here at startup is fatal
            // to this worker slot but does not crash the app - the
            // remaining workers still drain the queue.
            let _ = thread::Builder::new()
                .name("traceless-worker".to_string())
                .spawn(move || worker_loop(&rx));
        }
        Self { tx }
    }
}

fn worker_loop(rx: &Receiver<Job>) {
    while let Ok(job) = rx.recv_blocking() {
        // A panic inside a handler on one file must not kill the
        // worker. `catch_unwind` requires the payload to be
        // `UnwindSafe`; `Box<dyn FnOnce>` isn't on its own, so
        // `AssertUnwindSafe` tells the compiler we have thought
        // about it and the job does not leave shared state in a
        // broken invariant state. (The jobs this pool runs only
        // touch per-file temp paths and emit events through a
        // `Sender`; they do not hold locks or write to shared
        // data.)
        let result = catch_unwind(AssertUnwindSafe(job));
        if let Err(payload) = result {
            let msg = payload
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            log::error!("traceless worker job panicked: {msg}");
        }
    }
}

fn pool() -> &'static WorkerPool {
    static POOL: OnceLock<WorkerPool> = OnceLock::new();
    POOL.get_or_init(WorkerPool::new)
}

/// Queue a job for the shared worker pool. Returns immediately.
/// Submit is lock-free and safe to call from any thread.
///
/// The job runs on one of the pool's workers. Panics inside the job
/// are caught and logged rather than killing the worker.
pub fn submit<F>(job: F)
where
    F: FnOnce() + Send + 'static,
{
    // Unbounded channel: submit never blocks. Queueing memory is ~200
    // bytes per job (a boxed FnOnce), so even a million queued jobs
    // costs a few hundred MiB - much cheaper than a million 2 MiB
    // thread stacks.
    let _ = pool().tx.send_blocking(Box::new(job));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn submit_runs_many_jobs_without_thread_exhaustion() {
        // 500 jobs exceeds any reasonable per-file worker count and
        // well exceeds the pool's MAX_WORKERS cap, so this exercises
        // the queue behind a fixed-size pool. The old per-file spawn
        // would have tried to create 500 OS threads up-front.
        let counter = Arc::new(AtomicUsize::new(0));
        for _ in 0..500 {
            let c = counter.clone();
            submit(move || {
                c.fetch_add(1, Ordering::Relaxed);
            });
        }
        // Spin-wait up to 5 s for every job to complete. The jobs
        // are trivial so this finishes in milliseconds; the 5 s cap
        // is a hang guard, not a performance target.
        let deadline = Instant::now() + Duration::from_secs(5);
        while counter.load(Ordering::Relaxed) < 500 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(counter.load(Ordering::Relaxed), 500);
    }

    #[test]
    fn panicking_job_does_not_kill_pool() {
        // A handler bug that panics mid-clean must not shrink the
        // pool. Submit a panicking job, then submit 20 normal jobs
        // and verify they still run.
        submit(|| {
            panic!("synthetic panic for test");
        });

        let counter = Arc::new(AtomicUsize::new(0));
        for _ in 0..20 {
            let c = counter.clone();
            submit(move || {
                c.fetch_add(1, Ordering::Relaxed);
            });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while counter.load(Ordering::Relaxed) < 20 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(counter.load(Ordering::Relaxed), 20);
    }
}
