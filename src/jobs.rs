//! Shared helpers for polling background Tokio tasks without blocking the UI loop.

use anyhow::Result;
use tokio::task::JoinHandle;

pub(crate) enum PollJob<T> {
    Pending(JoinHandle<Result<T>>),
    Done(Result<T>),
}

pub(crate) async fn poll_job<T>(handle: JoinHandle<Result<T>>) -> PollJob<T> {
    if !handle.is_finished() {
        return PollJob::Pending(handle);
    }
    PollJob::Done(
        handle
            .await
            .unwrap_or_else(|error| Err(anyhow::anyhow!("task failed: {error}"))),
    )
}
