//! Per-session work queue — guarantees FIFO execution within a session key
//! while allowing concurrent execution across different sessions.

use dashmap::DashMap;
use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// A boxed future representing a unit of work to execute.
type WorkItem = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Per-key sequential work queue.
///
/// Each unique key gets a dedicated handler task that processes work items
/// one at a time in FIFO order. Different keys process concurrently.
///
/// Idle handlers self-terminate after 5 minutes and clean up their DashMap entry.
/// On shutdown (cancellation token), remaining items are drained before exit.
#[derive(Debug)]
pub struct SessionQueue<K: Hash + Eq + Clone + Send + Sync + Debug + 'static> {
    senders: Arc<DashMap<K, mpsc::UnboundedSender<WorkItem>>>,
    shutdown: CancellationToken,
}

impl<K: Hash + Eq + Clone + Send + Sync + Debug + 'static> SessionQueue<K> {
    pub fn new(shutdown: CancellationToken) -> Self {
        Self {
            senders: Arc::new(DashMap::new()),
            shutdown,
        }
    }

    /// Enqueue a work item for the given key.
    ///
    /// If no handler exists for this key, one is spawned. The work item
    /// will execute after any previously enqueued items for the same key.
    pub fn enqueue(&self, key: K, work: WorkItem) {
        // First attempt: look up existing sender.
        if let Some(sender) = self.senders.get(&key) {
            match sender.send(work) {
                Ok(()) => return,
                Err(mpsc::error::SendError(returned_work)) => {
                    // Handler exited (idle timeout race) — drop ref, remove stale
                    // entry, and fall through to spawn a new handler.
                    drop(sender);
                    self.senders.remove(&key);
                    self.spawn_handler(key, returned_work);
                    return;
                }
            }
        }

        // No sender — create a new channel + handler.
        self.spawn_handler(key, work);
    }

    fn spawn_handler(&self, key: K, first_item: WorkItem) {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(first_item); // cannot fail — we hold the rx

        let senders = Arc::clone(&self.senders);
        let shutdown = self.shutdown.clone();
        let handler_key = key.clone();
        tokio::spawn(async move {
            handler_loop(handler_key, rx, senders, shutdown).await;
        });

        self.senders.insert(key, tx);
    }
}

/// Sequential handler loop for a single key.
///
/// Processes work items one at a time. Exits on:
/// - All senders dropped (channel closed)
/// - Idle timeout (5 minutes with no work)
/// - Shutdown signal (drains remaining items first)
async fn handler_loop<K: Hash + Eq + Clone + Send + Sync + Debug + 'static>(
    key: K,
    mut rx: mpsc::UnboundedReceiver<WorkItem>,
    senders: Arc<DashMap<K, mpsc::UnboundedSender<WorkItem>>>,
    shutdown: CancellationToken,
) {
    let idle_timeout = Duration::from_secs(300);

    loop {
        tokio::select! {
            biased; // prefer shutdown over new work

            _ = shutdown.cancelled() => {
                debug!(key = ?key, "Session queue handler shutting down, draining remaining items");
                rx.close();
                while let Some(work) = rx.recv().await {
                    work.await;
                }
                break;
            }

            result = timeout(idle_timeout, rx.recv()) => {
                match result {
                    Ok(Some(work)) => {
                        work.await;
                    }
                    Ok(None) => {
                        // All senders dropped — channel closed.
                        debug!(key = ?key, "Session queue handler exiting: channel closed");
                        break;
                    }
                    Err(_) => {
                        // Idle timeout — self-cleanup.
                        debug!(key = ?key, "Session queue handler exiting: idle timeout");
                        senders.remove(&key);
                        // Drain any items that arrived between timeout and remove.
                        while let Ok(work) = rx.try_recv() {
                            work.await;
                        }
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn same_key_executes_sequentially() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::<u64>::new(shutdown);

        // Track execution order: each work item records when it starts and ends.
        let counter = Arc::new(AtomicUsize::new(0));
        let (tx, mut rx) = mpsc::unbounded_channel::<(usize, usize)>();

        for _ in 0..3 {
            let c = Arc::clone(&counter);
            let tx = tx.clone();
            queue.enqueue(
                1,
                Box::pin(async move {
                    let start = c.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let end = c.fetch_add(1, Ordering::SeqCst);
                    let _ = tx.send((start, end));
                }),
            );
        }
        drop(tx);

        let mut results = Vec::new();
        while let Some(r) = rx.recv().await {
            results.push(r);
        }

        assert_eq!(results.len(), 3);
        // Each item should start after the previous one ended.
        // Item 0: start=0, end=1. Item 1: start=2, end=3. Item 2: start=4, end=5.
        for i in 1..results.len() {
            assert!(
                results[i].0 > results[i - 1].1,
                "Item {} started ({}) before item {} ended ({})",
                i,
                results[i].0,
                i - 1,
                results[i - 1].1
            );
        }
    }

    #[tokio::test]
    async fn different_keys_execute_concurrently() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::<u64>::new(shutdown);

        // Use a barrier so both tasks must be running at the same time to proceed.
        let barrier = Arc::new(Barrier::new(2));
        let (tx, mut rx) = mpsc::unbounded_channel::<u64>();

        for key in [1u64, 2u64] {
            let b = Arc::clone(&barrier);
            let tx = tx.clone();
            queue.enqueue(
                key,
                Box::pin(async move {
                    b.wait().await; // both must reach here concurrently
                    let _ = tx.send(key);
                }),
            );
        }
        drop(tx);

        let mut results = Vec::new();
        while let Some(r) = rx.recv().await {
            results.push(r);
        }
        results.sort();
        assert_eq!(results, vec![1, 2]);
    }

    #[tokio::test]
    async fn idle_timeout_cleans_up() {
        tokio::time::pause();

        let shutdown = CancellationToken::new();
        let queue = SessionQueue::<u64>::new(shutdown);

        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        queue.enqueue(
            42,
            Box::pin(async move {
                let _ = tx.send(());
            }),
        );

        // Wait for the work item to complete.
        rx.recv().await;

        // Advance past idle timeout.
        tokio::time::advance(Duration::from_secs(301)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Handler should have cleaned up.
        assert!(
            !queue.senders.contains_key(&42),
            "DashMap entry should be removed after idle timeout"
        );
    }

    #[tokio::test]
    async fn re_enqueue_after_idle_eviction() {
        tokio::time::pause();

        let shutdown = CancellationToken::new();
        let queue = SessionQueue::<u64>::new(shutdown);

        let (tx1, mut rx1) = mpsc::unbounded_channel::<u8>();
        queue.enqueue(
            1,
            Box::pin(async move {
                let _ = tx1.send(1);
            }),
        );
        rx1.recv().await;

        // Evict via idle timeout.
        tokio::time::advance(Duration::from_secs(301)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Re-enqueue — should spawn new handler.
        let (tx2, mut rx2) = mpsc::unbounded_channel::<u8>();
        queue.enqueue(
            1,
            Box::pin(async move {
                let _ = tx2.send(2);
            }),
        );

        let val = rx2.recv().await.unwrap();
        assert_eq!(val, 2);
    }

    #[tokio::test]
    async fn shutdown_drains_remaining() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::<u64>::new(shutdown.clone());
        let executed = Arc::new(AtomicUsize::new(0));

        // Enqueue items that block until shutdown.
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let e = Arc::clone(&executed);
        queue.enqueue(
            1,
            Box::pin(async move {
                // Block until gate opens.
                let _ = gate_rx.await;
                e.fetch_add(1, Ordering::SeqCst);
            }),
        );

        let e = Arc::clone(&executed);
        queue.enqueue(
            1,
            Box::pin(async move {
                e.fetch_add(1, Ordering::SeqCst);
            }),
        );

        // Signal shutdown and unblock the first item.
        shutdown.cancel();
        let _ = gate_tx.send(());

        // Give handler time to drain.
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            executed.load(Ordering::SeqCst),
            2,
            "Both items should have been drained on shutdown"
        );
    }
}
