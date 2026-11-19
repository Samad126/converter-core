//! Concurrency pools. Documents and media are separate so a long transcode
//! cannot starve ordinary conversions (same rule as the TS backend).
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct Pool(Arc<Semaphore>);

impl Pool {
    pub fn new(slots: usize) -> Self {
        Self(Arc::new(Semaphore::new(slots.max(1))))
    }
    pub async fn acquire(&self) -> OwnedSemaphorePermit {
        self.0.clone().acquire_owned().await.expect("pool never closed")
    }
}
