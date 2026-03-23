//! Per-session work queue — guarantees FIFO execution within a session key
//! while allowing concurrent execution across different sessions.
//!
//! Supports two priority levels: normal (user runs) and low (notification runs).
//! Low-priority items only execute when no normal-priority items are pending.
//! Under sustained normal-priority load, low-priority items are intentionally
//! starved — user messages always take precedence over notifications.

use dashmap::DashMap;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// A boxed future representing a unit of work to execute.
type WorkItem = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A tagged work item with priority.
enum PriorityItem {
    Normal(WorkItem),
    Low(WorkItem),
}

/// Per-key sequential work queue.
///
/// Each unique key gets a dedicated handler task that processes work items
/// one at a time. Normal-priority items are processed before low-priority
/// ones. Different keys process concurrently.
///
/// Idle handlers self-terminate after 5 minutes and clean up their DashMap entry.
/// On shutdown (cancellation token), remaining items are drained before exit.
#[derive(Debug)]
pub struct SessionQueue<K: Hash + Eq + Clone + Send + Sync + Debug + 'static> {
    senders: Arc<DashMap<K, mpsc::UnboundedSender<PriorityItem>>>,
    /// Tracks how many items are pending (enqueued but not yet started) per key.
    pending_counts: Arc<DashMap<K, Arc<AtomicUsize>>>,
    shutdown: CancellationToken,
}

impl<K: Hash + Eq + Clone + Send + Sync + Debug + 'static> SessionQueue<K> {
    pub fn new(shutdown: CancellationToken) -> Self {
        Self {
            senders: Arc::new(DashMap::new()),
            pending_counts: Arc::new(DashMap::new()),
            shutdown,
        }
    }

    /// Enqueue a normal-priority work item (user runs).
    pub fn enqueue(&self, key: K, work: WorkItem) {
        self.enqueue_item(key, PriorityItem::Normal(work));
    }

    /// Enqueue a low-priority work item (notification runs).
    /// Low-priority items execute only when no normal items are pending.
    pub fn enqueue_low(&self, key: K, work: WorkItem) {
        self.enqueue_item(key, PriorityItem::Low(work));
    }

    /// Returns the number of items currently pending (enqueued but not yet
    /// started) for the given key.
    pub fn pending_count(&self, key: &K) -> usize {
        self.pending_counts
            .get(key)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn enqueue_item(&self, key: K, item: PriorityItem) {
        // Increment pending count.
        self.pending_counts
            .entry(key.clone())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .fetch_add(1, Ordering::Relaxed);

        if let Some(sender) = self.senders.get(&key) {
            match sender.send(item) {
                Ok(()) => return,
                Err(mpsc::error::SendError(returned_item)) => {
                    drop(sender);
                    self.senders.remove(&key);
                    self.spawn_handler(key, returned_item);
                    return;
                }
            }
        }
        self.spawn_handler(key, item);
    }

    fn spawn_handler(&self, key: K, first_item: PriorityItem) {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(first_item);

        let senders = Arc::clone(&self.senders);
        let pending = self
            .pending_counts
            .entry(key.clone())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone();
        let shutdown = self.shutdown.clone();
        let handler_key = key.clone();
        tokio::spawn(async move {
            handler_loop(handler_key, rx, senders, pending, shutdown).await;
        });

        self.senders.insert(key, tx);
    }
}

/// Sequential handler loop for a single key with priority support.
///
/// Drains all normal-priority items before processing any low-priority ones.
/// This ensures user messages are always processed before notification runs.
async fn handler_loop<K: Hash + Eq + Clone + Send + Sync + Debug + 'static>(
    key: K,
    mut rx: mpsc::UnboundedReceiver<PriorityItem>,
    senders: Arc<DashMap<K, mpsc::UnboundedSender<PriorityItem>>>,
    pending: Arc<AtomicUsize>,
    shutdown: CancellationToken,
) {
    let idle_timeout = Duration::from_secs(300);
    let mut low_queue: VecDeque<WorkItem> = VecDeque::new();

    loop {
        // First: drain any pending normal items before touching low-priority.
        // try_recv is non-blocking — picks up items that arrived while we
        // were executing the previous work item.
        let mut got_normal = false;
        while let Ok(item) = rx.try_recv() {
            match item {
                PriorityItem::Normal(work) => {
                    pending.fetch_sub(1, Ordering::Relaxed);
                    work.await;
                    got_normal = true;
                }
                PriorityItem::Low(work) => {
                    low_queue.push_back(work);
                }
            }
        }

        // If we processed normal items, loop back to check for more
        // (they may have arrived while we were awaiting).
        if got_normal {
            continue;
        }

        // No normal items pending — process one low-priority item if available.
        if let Some(low_work) = low_queue.pop_front() {
            pending.fetch_sub(1, Ordering::Relaxed);
            low_work.await;
            continue;
        }

        // Nothing pending — wait for new items (with timeout and shutdown).
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                debug!(key = ?key, "Session queue handler shutting down, draining remaining items");
                rx.close();
                while let Some(item) = rx.recv().await {
                    pending.fetch_sub(1, Ordering::Relaxed);
                    match item {
                        PriorityItem::Normal(work) | PriorityItem::Low(work) => work.await,
                    }
                }
                for work in low_queue.drain(..) {
                    work.await;
                }
                break;
            }

            result = timeout(idle_timeout, rx.recv()) => {
                match result {
                    Ok(Some(PriorityItem::Normal(work))) => {
                        pending.fetch_sub(1, Ordering::Relaxed);
                        work.await;
                    }
                    Ok(Some(PriorityItem::Low(work))) => {
                        // Defer: loop back to check for normal items first.
                        // Don't decrement yet — item hasn't started executing.
                        low_queue.push_back(work);
                    }
                    Ok(None) => {
                        // Channel closed — drain any remaining low-priority items.
                        debug!(key = ?key, "Session queue handler exiting: channel closed");
                        for work in low_queue.drain(..) {
                            work.await;
                        }
                        senders.remove(&key);
                        break;
                    }
                    Err(_) => {
                        debug!(key = ?key, "Session queue handler exiting: idle timeout");
                        senders.remove(&key);
                        while let Ok(item) = rx.try_recv() {
                            pending.fetch_sub(1, Ordering::Relaxed);
                            match item {
                                PriorityItem::Normal(work) | PriorityItem::Low(work) => work.await,
                            }
                        }
                        for work in low_queue.drain(..) {
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

        let barrier = Arc::new(Barrier::new(2));
        let (tx, mut rx) = mpsc::unbounded_channel::<u64>();

        for key in [1u64, 2u64] {
            let b = Arc::clone(&barrier);
            let tx = tx.clone();
            queue.enqueue(
                key,
                Box::pin(async move {
                    b.wait().await;
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
    async fn low_priority_deferred_behind_normal() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::<u64>::new(shutdown);

        let (tx, mut rx) = mpsc::unbounded_channel::<&'static str>();

        // Enqueue: normal blocks on gate, then low, then normal.
        // Expected order: normal1, normal2, low
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();

        let tx1 = tx.clone();
        queue.enqueue(
            1,
            Box::pin(async move {
                let _ = gate_rx.await; // block until gate opens
                let _ = tx1.send("normal1");
            }),
        );

        // Yield to let the handler task start processing normal1 (blocked on gate)
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let tx2 = tx.clone();
        queue.enqueue_low(
            1,
            Box::pin(async move {
                let _ = tx2.send("low");
            }),
        );

        let tx3 = tx.clone();
        queue.enqueue(
            1,
            Box::pin(async move {
                let _ = tx3.send("normal2");
            }),
        );

        // Unblock normal1
        let _ = gate_tx.send(());
        drop(tx);

        let mut results = Vec::new();
        while let Some(r) = rx.recv().await {
            results.push(r);
        }

        assert_eq!(
            results,
            vec!["normal1", "normal2", "low"],
            "Low-priority item should execute after all normal items"
        );
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

        rx.recv().await;

        tokio::time::advance(Duration::from_secs(301)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

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

        tokio::time::advance(Duration::from_secs(301)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

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

        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let e = Arc::clone(&executed);
        queue.enqueue(
            1,
            Box::pin(async move {
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

        shutdown.cancel();
        let _ = gate_tx.send(());

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            executed.load(Ordering::SeqCst),
            2,
            "Both items should have been drained on shutdown"
        );
    }
}
