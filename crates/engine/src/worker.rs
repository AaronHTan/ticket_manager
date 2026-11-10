//! Asynchronous triage: a bounded queue feeding a pool of workers.

use std::sync::Arc;

use ticket_core::{Error, Result, TicketId};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::pipeline::TriagePipeline;

/// A cloneable handle for submitting tickets for triage.
///
/// The queue only holds ids. The ticket itself is already persisted with
/// status `received`, so a lost queue entry is recovered on the next start
/// (see [`TriagePipeline::recover`]). A durable queue (a DB outbox, SQS or
/// NATS) can replace this without changing the pipeline.
#[derive(Debug, Clone)]
pub struct TriageQueue {
    tx: mpsc::Sender<TicketId>,
}

impl TriageQueue {
    /// Enqueues without waiting. Fails with [`Error::Unavailable`] when the
    /// queue is full or shutting down, which applies back-pressure to callers.
    pub fn try_enqueue(&self, id: TicketId) -> Result<()> {
        self.tx.try_send(id).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => Error::Unavailable("triage queue is full".into()),
            mpsc::error::TrySendError::Closed(_) => {
                Error::Unavailable("triage queue is shut down".into())
            }
        })
    }
}

/// The running workers.
#[derive(Debug)]
pub struct WorkerPool {
    handles: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    /// Spawns `workers` tasks sharing a queue of `capacity` entries.
    ///
    /// Workers exit once every [`TriageQueue`] handle is dropped and the queue
    /// is drained, so dropping the handles gives a graceful shutdown.
    pub fn spawn(
        pipeline: Arc<TriagePipeline>,
        workers: usize,
        capacity: usize,
    ) -> (TriageQueue, Self) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let rx = Arc::new(Mutex::new(rx));
        let handles = (0..workers.max(1))
            .map(|worker| {
                let rx = Arc::clone(&rx);
                let pipeline = Arc::clone(&pipeline);
                tokio::spawn(async move {
                    loop {
                        // The guard drops at the end of this statement, so only one idle
                        // worker waits on `recv` and the others wait on the lock.
                        let next = rx.lock().await.recv().await;
                        let Some(id) = next else { break };
                        if let Err(error) = pipeline.process(id).await {
                            error!(worker, ticket = %id, %error, "failed to process ticket");
                        }
                    }
                    debug!(worker, "worker stopped");
                })
            })
            .collect();
        (TriageQueue { tx }, Self { handles })
    }

    /// Waits for every worker to finish.
    pub async fn join(self) {
        for handle in self.handles {
            if let Err(error) = handle.await {
                error!(%error, "worker panicked");
            }
        }
        info!("all triage workers stopped");
    }
}
