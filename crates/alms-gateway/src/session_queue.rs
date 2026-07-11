//! Bounded per-key work scheduling.
//!
//! Queue slots are created with one atomic DashMap entry operation, so
//! concurrent first submissions for one key cannot create two workers.

use dashmap::{DashMap, mapref::entry::Entry};
use futures::FutureExt;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::fmt::{self, Debug};
use std::future::Future;
use std::hash::Hash;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub const MAX_PENDING_PER_KEY: usize = 64;
pub const MAX_PENDING_TOTAL: usize = 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub type WorkItem = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    PerKeyFull,
    GlobalFull,
    ShuttingDown,
    DispatchClosed,
}

#[derive(Debug, Clone, Copy)]
struct QueueLimits {
    per_key: usize,
    total: usize,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            per_key: MAX_PENDING_PER_KEY,
            total: MAX_PENDING_TOTAL,
        }
    }
}

struct AdmittedItem {
    work: WorkItem,
    _key_permit: OwnedSemaphorePermit,
    _global_permit: OwnedSemaphorePermit,
}

enum PriorityItem {
    Normal(AdmittedItem),
    Low(AdmittedItem),
}

#[derive(Default)]
struct PendingCounts {
    normal: usize,
    low: usize,
}

impl PendingCounts {
    fn total(&self) -> usize {
        self.normal + self.low
    }
}

/// One key's dispatch state.
///
/// Idle retirement and shutdown drain rely on the slot having exactly the
/// map reference plus the worker reference when unused. Do not retain an
/// extra `Arc<QueueSlot>` outside admission/reservation paths.
struct QueueSlot {
    sender: mpsc::Sender<PriorityItem>,
    capacity: Arc<Semaphore>,
    pending: Mutex<PendingCounts>,
    changed: Notify,
}

struct QueueInner<K> {
    slots: DashMap<K, Arc<QueueSlot>>,
    global_capacity: Arc<Semaphore>,
    limits: QueueLimits,
    shutdown: CancellationToken,
}

struct SlotUse {
    slot: Arc<QueueSlot>,
}

impl Drop for SlotUse {
    fn drop(&mut self) {
        self.slot.changed.notify_one();
    }
}

/// Capacity atomically admitted for one queue key.
///
/// Callers submit synchronously before their next await. Dropping a
/// reservation releases both capacity permits.
pub struct Reservation {
    slot: Arc<QueueSlot>,
    key_permit: Option<OwnedSemaphorePermit>,
    global_permit: Option<OwnedSemaphorePermit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchReceipt {
    queued_ahead: usize,
}

impl DispatchReceipt {
    pub fn queued_ahead(&self) -> usize {
        self.queued_ahead
    }
}

impl Debug for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reservation").finish_non_exhaustive()
    }
}

impl Reservation {
    pub fn submit(self, work: WorkItem) -> Result<DispatchReceipt, AdmissionError> {
        self.dispatch(work, false)
    }

    pub fn submit_low(self, work: WorkItem) -> Result<DispatchReceipt, AdmissionError> {
        self.dispatch(work, true)
    }

    fn dispatch(mut self, work: WorkItem, low: bool) -> Result<DispatchReceipt, AdmissionError> {
        let key = self.key_permit.take().expect("key permit");
        let global = self.global_permit.take().expect("global permit");
        let item = AdmittedItem {
            work,
            _key_permit: key,
            _global_permit: global,
        };
        let item = if low {
            PriorityItem::Low(item)
        } else {
            PriorityItem::Normal(item)
        };

        let mut pending = self.slot.pending.lock();
        let queued_ahead = if low {
            let ahead = pending.total();
            pending.low += 1;
            ahead
        } else {
            let ahead = pending.normal;
            pending.normal += 1;
            ahead
        };
        if self.slot.sender.try_send(item).is_err() {
            if low {
                pending.low -= 1;
            } else {
                pending.normal -= 1;
            }
            return Err(AdmissionError::DispatchClosed);
        }
        drop(pending);
        Ok(DispatchReceipt { queued_ahead })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.slot.changed.notify_one();
    }
}

pub struct SessionQueue<K>
where
    K: Hash + Eq + Clone + Send + Sync + Debug + 'static,
{
    inner: Arc<QueueInner<K>>,
}

impl<K> Debug for SessionQueue<K>
where
    K: Hash + Eq + Clone + Send + Sync + Debug + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionQueue")
            .field("slots", &self.inner.slots.len())
            .field("limits", &self.inner.limits)
            .finish()
    }
}

impl<K> SessionQueue<K>
where
    K: Hash + Eq + Clone + Send + Sync + Debug + 'static,
{
    pub fn new(shutdown: CancellationToken) -> Self {
        Self::with_queue_limits(shutdown, QueueLimits::default())
    }

    fn with_queue_limits(shutdown: CancellationToken, limits: QueueLimits) -> Self {
        assert!(limits.per_key > 0, "per-key queue limit must be positive");
        assert!(limits.total > 0, "global queue limit must be positive");
        Self {
            inner: Arc::new(QueueInner {
                slots: DashMap::new(),
                global_capacity: Arc::new(Semaphore::new(limits.total)),
                limits,
                shutdown,
            }),
        }
    }

    /// Attempt admission without waiting.
    pub fn try_reserve(&self, key: K) -> Result<Reservation, AdmissionError> {
        if self.inner.shutdown.is_cancelled() {
            return Err(AdmissionError::ShuttingDown);
        }

        let slot = SlotUse {
            slot: self.slot_for(key),
        };
        let key_permit = slot
            .slot
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdmissionError::PerKeyFull)?;
        let global_permit = self
            .inner
            .global_capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdmissionError::GlobalFull)?;

        if self.inner.shutdown.is_cancelled() {
            return Err(AdmissionError::ShuttingDown);
        }

        Ok(Self::reservation(
            Arc::clone(&slot.slot),
            key_permit,
            global_permit,
        ))
    }

    /// Wait for bounded admission.
    ///
    /// Per-key capacity is acquired before global capacity so one saturated
    /// key cannot reserve the global pool while it waits.
    ///
    /// This waiting API must never be called from inside a work item on this
    /// queue. A work item waiting for capacity on its own saturated key would
    /// prevent the worker from advancing and releasing that capacity.
    pub async fn reserve(&self, key: K) -> Result<Reservation, AdmissionError> {
        if self.inner.shutdown.is_cancelled() {
            return Err(AdmissionError::ShuttingDown);
        }

        let slot = SlotUse {
            slot: self.slot_for(key),
        };
        let key_permit = tokio::select! {
            permit = slot.slot.capacity.clone().acquire_owned() => {
                permit.map_err(|_| AdmissionError::ShuttingDown)?
            }
            _ = self.inner.shutdown.cancelled() => {
                return Err(AdmissionError::ShuttingDown);
            }
        };
        let global_permit = tokio::select! {
            permit = self.inner.global_capacity.clone().acquire_owned() => {
                permit.map_err(|_| AdmissionError::ShuttingDown)?
            }
            _ = self.inner.shutdown.cancelled() => {
                return Err(AdmissionError::ShuttingDown);
            }
        };

        if self.inner.shutdown.is_cancelled() {
            return Err(AdmissionError::ShuttingDown);
        }

        Ok(Self::reservation(
            Arc::clone(&slot.slot),
            key_permit,
            global_permit,
        ))
    }

    /// Accepted items for a key that have not started.
    #[cfg(test)]
    pub fn pending_count(&self, key: &K) -> usize {
        self.inner
            .slots
            .get(key)
            .map(|slot| slot.pending.lock().total())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn enqueue(&self, key: K, work: WorkItem) {
        self.try_reserve(key)
            .expect("test queue admission")
            .submit(work)
            .expect("test queue dispatch");
    }

    #[cfg(test)]
    pub fn enqueue_low(&self, key: K, work: WorkItem) {
        self.try_reserve(key)
            .expect("test queue admission")
            .submit_low(work)
            .expect("test queue dispatch");
    }

    fn reservation(
        slot: Arc<QueueSlot>,
        key_permit: OwnedSemaphorePermit,
        global_permit: OwnedSemaphorePermit,
    ) -> Reservation {
        Reservation {
            slot,
            key_permit: Some(key_permit),
            global_permit: Some(global_permit),
        }
    }

    fn slot_for(&self, key: K) -> Arc<QueueSlot> {
        match self.inner.slots.entry(key.clone()) {
            Entry::Occupied(entry) => Arc::clone(entry.get()),
            Entry::Vacant(entry) => {
                let (sender, receiver) = mpsc::channel(self.inner.limits.per_key);
                let slot = Arc::new(QueueSlot {
                    sender,
                    capacity: Arc::new(Semaphore::new(self.inner.limits.per_key)),
                    pending: Mutex::new(PendingCounts::default()),
                    changed: Notify::new(),
                });
                entry.insert(Arc::clone(&slot));
                tokio::spawn(queue_worker(
                    key,
                    Arc::clone(&slot),
                    receiver,
                    Arc::clone(&self.inner),
                ));
                slot
            }
        }
    }

    #[cfg(test)]
    fn for_test(shutdown: CancellationToken, per_key: usize, total: usize) -> Self {
        Self::with_queue_limits(shutdown, QueueLimits { per_key, total })
    }

    #[cfg(test)]
    fn pending_total(&self) -> usize {
        self.inner.limits.total - self.inner.global_capacity.available_permits()
    }

    #[cfg(test)]
    fn slot_count(&self) -> usize {
        self.inner.slots.len()
    }
}

async fn execute_item(slot: &QueueSlot, item: AdmittedItem, low: bool) {
    let AdmittedItem {
        work,
        _key_permit: key_permit,
        _global_permit: global_permit,
    } = item;
    {
        let mut pending = slot.pending.lock();
        if low {
            pending.low -= 1;
        } else {
            pending.normal -= 1;
        }
    }
    slot.changed.notify_one();
    drop(key_permit);
    drop(global_permit);
    if AssertUnwindSafe(work).catch_unwind().await.is_err() {
        tracing::error!("Session queue work item panicked; continuing worker");
    }
}

fn push_item(
    item: PriorityItem,
    normal: &mut VecDeque<AdmittedItem>,
    low: &mut VecDeque<AdmittedItem>,
) {
    match item {
        PriorityItem::Normal(item) => normal.push_back(item),
        PriorityItem::Low(item) => low.push_back(item),
    }
}

fn retire_idle_slot<K>(inner: &QueueInner<K>, key: &K, slot: &Arc<QueueSlot>) -> bool
where
    K: Hash + Eq,
{
    inner
        .slots
        .remove_if(key, |_, current| {
            Arc::ptr_eq(current, slot)
                && current.pending.lock().total() == 0
                // The map and worker are the only owners when no admission
                // call, waiting admission, or reservation is in flight.
                && Arc::strong_count(current) == 2
        })
        .is_some()
}

fn remove_shutdown_slot<K>(inner: &QueueInner<K>, key: &K, slot: &Arc<QueueSlot>)
where
    K: Hash + Eq,
{
    inner
        .slots
        .remove_if(key, |_, current| Arc::ptr_eq(current, slot));
}

async fn queue_worker<K>(
    key: K,
    slot: Arc<QueueSlot>,
    mut receiver: mpsc::Receiver<PriorityItem>,
    inner: Arc<QueueInner<K>>,
) where
    K: Hash + Eq + Clone + Send + Sync + Debug + 'static,
{
    let mut normal = VecDeque::new();
    let mut low = VecDeque::new();
    let mut shutting_down = false;

    loop {
        while let Ok(item) = receiver.try_recv() {
            push_item(item, &mut normal, &mut low);
        }

        if let Some(item) = normal.pop_front() {
            execute_item(&slot, item, false).await;
            continue;
        }
        if let Some(item) = low.pop_front() {
            execute_item(&slot, item, true).await;
            continue;
        }

        if shutting_down {
            let changed = slot.changed.notified();
            if slot.pending.lock().total() == 0 && Arc::strong_count(&slot) == 2 {
                receiver.close();
                debug!(key = ?key, "Session queue drained during shutdown");
                remove_shutdown_slot(&inner, &key, &slot);
                break;
            }

            tokio::select! {
                received = receiver.recv() => {
                    if let Some(item) = received {
                        push_item(item, &mut normal, &mut low);
                    }
                }
                _ = changed => {}
            }
            continue;
        }

        tokio::select! {
            biased;

            _ = inner.shutdown.cancelled() => {
                shutting_down = true;
            }

            received = timeout(IDLE_TIMEOUT, receiver.recv()) => {
                match received {
                    Ok(Some(item)) => push_item(item, &mut normal, &mut low),
                    Ok(None) => {
                        remove_shutdown_slot(&inner, &key, &slot);
                        break;
                    }
                    Err(_) => {
                        if retire_idle_slot(&inner, &key, &slot) {
                            debug!(key = ?key, "Session queue retired after idle timeout");
                            break;
                        }
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
    use tokio::sync::{Barrier, oneshot};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_admission_creates_one_serial_worker() {
        const ITEMS: usize = 128;
        let queue = Arc::new(SessionQueue::for_test(
            CancellationToken::new(),
            ITEMS,
            ITEMS,
        ));
        let start = Arc::new(Barrier::new(ITEMS + 1));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (done_tx, mut done_rx) = mpsc::channel(ITEMS);
        let mut producers = Vec::new();

        for _ in 0..ITEMS {
            let queue = Arc::clone(&queue);
            let start = Arc::clone(&start);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let done_tx = done_tx.clone();
            producers.push(tokio::spawn(async move {
                start.wait().await;
                queue
                    .try_reserve(7)
                    .expect("admission")
                    .submit(Box::pin(async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        done_tx.send(()).await.expect("done receiver");
                    }))
                    .expect("dispatch");
            }));
        }

        start.wait().await;
        for producer in producers {
            producer.await.expect("producer task");
        }
        drop(done_tx);
        for _ in 0..ITEMS {
            done_rx.recv().await.expect("completed item");
        }

        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(queue.slot_count(), 1);
    }

    #[tokio::test]
    async fn different_keys_execute_concurrently() {
        let queue = SessionQueue::for_test(CancellationToken::new(), 2, 4);
        let rendezvous = Arc::new(Barrier::new(3));

        for key in [1, 2] {
            let rendezvous = Arc::clone(&rendezvous);
            queue
                .try_reserve(key)
                .expect("admission")
                .submit(Box::pin(async move {
                    rendezvous.wait().await;
                }))
                .expect("dispatch");
        }

        rendezvous.wait().await;
    }

    #[tokio::test]
    async fn capacity_is_bounded_and_drop_rolls_back() {
        let queue = SessionQueue::for_test(CancellationToken::new(), 2, 3);
        let first = queue.try_reserve(1).expect("first");
        let second = queue.try_reserve(1).expect("second");
        assert_eq!(
            queue.try_reserve(1).expect_err("per-key full"),
            AdmissionError::PerKeyFull
        );

        let other_key = queue.try_reserve(2).expect("global final slot");
        assert_eq!(
            queue.try_reserve(3).expect_err("global full"),
            AdmissionError::GlobalFull
        );
        assert_eq!(queue.pending_total(), 3);

        drop(second);
        drop(other_key);
        assert_eq!(queue.pending_total(), 1);
        drop(first);
        assert_eq!(queue.pending_total(), 0);
    }

    #[tokio::test]
    async fn positions_linearize_at_submission_and_respect_priority() {
        let queue = SessionQueue::for_test(CancellationToken::new(), 4, 4);
        let first = queue.try_reserve(1).expect("first");
        let second = queue.try_reserve(1).expect("second");
        let first_receipt = first.submit(Box::pin(async {})).expect("first dispatch");
        let second_receipt = second.submit(Box::pin(async {})).expect("second dispatch");
        assert_eq!(first_receipt.queued_ahead(), 0);
        assert_eq!(second_receipt.queued_ahead(), 1);

        let low = queue.try_reserve(2).expect("low");
        let normal = queue.try_reserve(2).expect("normal");
        let low_receipt = low.submit_low(Box::pin(async {})).expect("low dispatch");
        let normal_receipt = normal.submit(Box::pin(async {})).expect("normal dispatch");
        assert_eq!(low_receipt.queued_ahead(), 0);
        assert_eq!(normal_receipt.queued_ahead(), 0);
    }

    #[tokio::test]
    async fn waiting_admission_proceeds_after_capacity_is_released() {
        let queue = Arc::new(SessionQueue::for_test(CancellationToken::new(), 1, 1));
        let held = queue.try_reserve(1).expect("held reservation");
        let waiter = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.reserve(1).await })
        };

        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(held);

        let admitted = waiter
            .await
            .expect("waiter task")
            .expect("waiting admission");
        drop(admitted);
        assert_eq!(queue.pending_total(), 0);
    }

    #[tokio::test]
    async fn normal_priority_overtakes_buffered_low_priority() {
        let queue = SessionQueue::for_test(CancellationToken::new(), 3, 3);
        let (gate_tx, gate_rx) = oneshot::channel();
        let (order_tx, mut order_rx) = mpsc::channel(3);

        let first_tx = order_tx.clone();
        queue
            .try_reserve(1)
            .expect("first")
            .submit(Box::pin(async move {
                let _ = gate_rx.await;
                first_tx.send("first").await.expect("order receiver");
            }))
            .expect("first dispatch");
        tokio::task::yield_now().await;

        let low_tx = order_tx.clone();
        queue
            .try_reserve(1)
            .expect("low")
            .submit_low(Box::pin(async move {
                low_tx.send("low").await.expect("order receiver");
            }))
            .expect("low dispatch");
        queue
            .try_reserve(1)
            .expect("normal")
            .submit(Box::pin(async move {
                order_tx.send("normal").await.expect("order receiver");
            }))
            .expect("normal dispatch");

        gate_tx.send(()).expect("release first");
        assert_eq!(order_rx.recv().await, Some("first"));
        assert_eq!(order_rx.recv().await, Some("normal"));
        assert_eq!(order_rx.recv().await, Some("low"));
    }

    #[tokio::test]
    async fn in_flight_reservation_prevents_idle_eviction() {
        tokio::time::pause();
        let queue = SessionQueue::for_test(CancellationToken::new(), 2, 2);
        let reservation = queue.try_reserve(9).expect("reservation");

        tokio::task::yield_now().await;
        tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(queue.slot_count(), 1);

        let (done_tx, done_rx) = oneshot::channel();
        reservation
            .submit(Box::pin(async move {
                done_tx.send(()).expect("done receiver");
            }))
            .expect("dispatch after idle boundary");
        done_rx.await.expect("work completed");

        tokio::task::yield_now().await;
        tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(queue.slot_count(), 0);
    }

    #[tokio::test]
    async fn stale_worker_cleanup_cannot_remove_replacement_slot() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::for_test(shutdown.clone(), 2, 2);
        let stale = queue.slot_for(5);
        queue.inner.slots.remove(&5);
        let replacement = queue.slot_for(5);
        assert!(!Arc::ptr_eq(&stale, &replacement));

        remove_shutdown_slot(&queue.inner, &5, &stale);
        let current = queue.inner.slots.get(&5).expect("replacement slot");
        assert!(Arc::ptr_eq(current.value(), &replacement));
        drop(current);
        shutdown.cancel();
    }

    #[tokio::test]
    async fn reservation_accepted_before_shutdown_can_still_submit() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::for_test(shutdown.clone(), 1, 1);
        let reservation = queue.try_reserve(1).expect("accepted reservation");
        let (done_tx, done_rx) = oneshot::channel();

        shutdown.cancel();
        tokio::task::yield_now().await;
        reservation
            .submit(Box::pin(async move {
                done_tx.send(()).expect("done receiver");
            }))
            .expect("accepted work must dispatch during drain");

        done_rx.await.expect("accepted work drained");
    }

    #[tokio::test]
    async fn shutdown_cleans_up_cancelled_admission_waiter() {
        let shutdown = CancellationToken::new();
        let queue = Arc::new(SessionQueue::for_test(shutdown.clone(), 1, 1));
        let held = queue.try_reserve(1).expect("held reservation");
        let waiter = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.reserve(1).await })
        };
        tokio::task::yield_now().await;

        shutdown.cancel();
        assert_eq!(
            waiter.await.expect("waiter task").expect_err("shutdown"),
            AdmissionError::ShuttingDown
        );
        drop(held);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(queue.slot_count(), 0);
    }

    #[tokio::test]
    async fn panicking_work_does_not_poison_key_worker() {
        let queue = SessionQueue::for_test(CancellationToken::new(), 2, 2);
        let (done_tx, done_rx) = oneshot::channel();
        queue
            .try_reserve(1)
            .expect("panic admission")
            .submit(Box::pin(async {
                panic!("intentional queue test panic");
            }))
            .expect("panic dispatch");
        queue
            .try_reserve(1)
            .expect("following admission")
            .submit(Box::pin(async move {
                done_tx.send(()).expect("done receiver");
            }))
            .expect("following dispatch");

        done_rx.await.expect("worker continued after panic");
    }

    #[tokio::test]
    async fn shutdown_preserves_priority_and_rejects_new_work() {
        let shutdown = CancellationToken::new();
        let queue = SessionQueue::for_test(shutdown.clone(), 3, 3);
        let (gate_tx, gate_rx) = oneshot::channel();
        let (order_tx, mut order_rx) = mpsc::channel(3);

        let first_tx = order_tx.clone();
        queue
            .try_reserve(1)
            .expect("first")
            .submit(Box::pin(async move {
                let _ = gate_rx.await;
                first_tx.send("first").await.expect("order receiver");
            }))
            .expect("first dispatch");
        tokio::task::yield_now().await;

        let low_tx = order_tx.clone();
        queue
            .try_reserve(1)
            .expect("low")
            .submit_low(Box::pin(async move {
                low_tx.send("low").await.expect("order receiver");
            }))
            .expect("low dispatch");
        queue
            .try_reserve(1)
            .expect("normal")
            .submit(Box::pin(async move {
                order_tx.send("normal").await.expect("order receiver");
            }))
            .expect("normal dispatch");

        shutdown.cancel();
        gate_tx.send(()).expect("release first");
        assert_eq!(order_rx.recv().await, Some("first"));
        assert_eq!(order_rx.recv().await, Some("normal"));
        assert_eq!(order_rx.recv().await, Some("low"));
        assert_eq!(
            queue.try_reserve(1).expect_err("shutdown rejection"),
            AdmissionError::ShuttingDown
        );
    }
}
