//! Bounded per-pane raw PTY output ring for the `pane.stream` firehose.
//!
//! A single producer (the PTY reader thread, via `process_pty_bytes` while it
//! already holds the terminal core lock) appends raw pre-parser bytes here.
//! One or more stream server threads drain the ring through independent
//! cursors. The ring is deliberately bounded: when the slowest consumer cannot
//! keep up the writer never blocks and never grows memory. Instead the oldest
//! bytes are evicted; a consumer whose cursor has fallen off the retained
//! window is told to `Lagged` and the server re-seeds it with a single
//! full-screen keyframe (snapshot-collapse resync).
//!
//! Correctness of the seed/resume boundary depends on the byte-offset advance
//! and the terminal screen mutation happening inside the *same* core-lock
//! critical section (see `PaneTerminal::process_pty_bytes` and
//! `PaneTerminal::output_snapshot`), so a captured `(visible_ansi, offset)`
//! pair is always a consistent cut with no gap or overlap.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use bytes::Bytes;

use super::terminal::PaneTerminal;

/// Default bounded capacity of a pane's output ring (~1 MiB of retained bytes).
pub(crate) const OUTPUT_RING_CAPACITY_BYTES: usize = 1024 * 1024;

/// Default and clamp bounds for the per-frame coalescing size.
pub(crate) const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;
const MIN_MAX_FRAME_BYTES: usize = 1024;

/// Hard ceiling on retained events, so tiny data writes interleaved with resizes
/// cannot grow the `VecDeque` unbounded even while under the byte capacity.
/// (Resize-only traffic is additionally coalesced to a single trailing marker.)
const MAX_RING_EVENTS: usize = 1 << 16;

/// One ordered event in the ring.
enum RingEvent {
    /// A run of raw PTY bytes beginning at absolute offset `start`.
    Data { start: u64, bytes: Bytes },
    /// A PTY resize interleaved in offset order at absolute byte position `at`
    /// (the offset it took effect). `id` is a monotonic per-ring counter used by
    /// consumers to deliver each resize exactly once, independent of the byte
    /// cursor. `at` keys the binary search that locates a consumer's cursor.
    Resize {
        id: u64,
        at: u64,
        cols: u16,
        rows: u16,
    },
}

/// One bounded step of drained output for a consumer.
///
/// A single `drain` call returns at most one frame's worth of work so the ring
/// lock is held only briefly and a stalled viewer retains at most one frame (not
/// the whole backlog). The serve loop re-wakes and drains the next step.
pub(crate) enum OutputDrain {
    /// Raw byte chunks — cheap `Bytes` slices that share the ring's buffers, so
    /// no payload is copied under the lock — whose total length is
    /// `<= max_frame_bytes`. The caller concatenates + base64s them into ONE
    /// `data` frame OUTSIDE the ring lock. `cursor` is the advanced byte offset.
    Data { chunks: Vec<Bytes>, cursor: u64 },
    /// A single resize marker to emit (in offset order, before following data).
    Resize {
        cols: u16,
        rows: u16,
        resize_id: u64,
    },
    /// The cursor fell off the retained window; the server must resync.
    Lagged,
    /// Nothing pending for this cursor right now.
    Idle,
}

/// Outcome of waiting for the ring to have work for a consumer.
pub(crate) enum OutputWait {
    /// There is data, a pending resize, or a lag to handle; drain now.
    Ready,
    /// The idle timeout elapsed; the server should heartbeat / poll the peer.
    Idle,
    /// The producing runtime is gone; the server should emit `exited`.
    Closed,
}

/// An atomically captured full-screen seed and the exact offset it reflects.
pub(crate) struct OutputSnapshot {
    pub ansi: String,
    /// Normal-screen scrollback rows ABOVE the screen in `ansi`, oldest first, at
    /// the same width. `None` when the stream did not ask for history.
    pub history: Option<String>,
    /// `pane.history` cursor for the first row of `history`, present only when
    /// older rows remain above it.
    pub history_cursor: Option<String>,
    pub cursor: u64,
    pub resize_id: u64,
    pub cols: u16,
    pub rows: u16,
}

struct OutputRingInner {
    events: VecDeque<RingEvent>,
    /// Absolute offset of the oldest retained data byte.
    head_offset: u64,
    /// Absolute offset just past the newest retained data byte.
    total_offset: u64,
    buffered_bytes: usize,
    capacity: usize,
    cols: u16,
    rows: u16,
    subscribers: usize,
    /// Highest resize id ever assigned (monotonic).
    resize_counter: u64,
    /// Highest resize id that has been *evicted*. A consumer whose delivered
    /// `resize_id` is below this missed a resize it needed and must resync — the
    /// resize analogue of `head_offset` advancing past a consumer's byte cursor.
    resize_floor: u64,
}

impl OutputRingInner {
    /// Whether a consumer at `resize_id` still has a *retained* (not evicted)
    /// undelivered resize. O(1): retained undelivered ids are exactly those in
    /// `(resize_id, resize_counter]` once `resize_id >= resize_floor` (evicted
    /// ids are `<= resize_floor`). A consumer below `resize_floor` is handled as
    /// a lag, not here.
    fn has_retained_pending_resize(&self, resize_id: u64) -> bool {
        resize_id >= self.resize_floor && self.resize_counter > resize_id
    }
}

/// A shared bounded ring of raw PTY output for one pane.
pub(crate) struct OutputRing {
    inner: Mutex<OutputRingInner>,
    ready: Condvar,
    epoch: u64,
    closed: AtomicBool,
    /// Weak so the ring never keeps the terminal (and therefore the PTY) alive.
    terminal: Weak<PaneTerminal>,
}

impl OutputRing {
    // `pub(super)` (constructed only by `PaneRuntime` in `crate::pane`) so the
    // `Weak<PaneTerminal>` parameter does not raise `PaneTerminal`'s effective
    // visibility to `pub(crate)` and trip `private_interfaces` on unrelated
    // terminal-core fields.
    pub(super) fn new(
        epoch: u64,
        capacity: usize,
        cols: u16,
        rows: u16,
        terminal: Weak<PaneTerminal>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(OutputRingInner {
                events: VecDeque::new(),
                head_offset: 0,
                total_offset: 0,
                buffered_bytes: 0,
                capacity: capacity.max(1),
                cols,
                rows,
                subscribers: 0,
                resize_counter: 0,
                resize_floor: 0,
            }),
            ready: Condvar::new(),
            epoch,
            closed: AtomicBool::new(false),
            terminal,
        })
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Mark the ring closed (the owning runtime is being dropped) and wake any
    /// waiting consumers so they can emit `exited`.
    pub(crate) fn mark_closed(&self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify_all();
    }

    /// Register a viewer. Returns the new subscriber count.
    pub(crate) fn add_subscriber(&self) -> usize {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.subscribers += 1;
        inner.subscribers
    }

    /// Unregister a viewer. Returns the remaining subscriber count.
    pub(crate) fn remove_subscriber(&self) -> usize {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.subscribers = inner.subscribers.saturating_sub(1);
        inner.subscribers
    }

    /// Append a run of raw PTY bytes.
    ///
    /// Called by the PTY reader thread from inside `process_pty_bytes` while the
    /// terminal core lock is held, so the offset advance here is serialized with
    /// the screen mutation. Never blocks on a consumer: on overflow the oldest
    /// retained bytes are evicted (a lagging consumer resyncs from a keyframe).
    pub(crate) fn append(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        let start = inner.total_offset;
        inner.total_offset = start.saturating_add(bytes.len() as u64);
        inner.buffered_bytes += bytes.len();
        inner.events.push_back(RingEvent::Data {
            start,
            bytes: Bytes::copy_from_slice(bytes),
        });
        evict_to_capacity(&mut inner);
        drop(inner);
        self.ready.notify_all();
    }

    /// Record a PTY resize marker interleaved at the current offset.
    ///
    /// Also called under the terminal core lock (from the resize path), so it is
    /// ordered against surrounding data appends.
    pub(crate) fn note_resize(&self, cols: u16, rows: u16) {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.cols = cols;
        inner.rows = rows;
        inner.resize_counter += 1;
        let id = inner.resize_counter;
        let at = inner.total_offset;
        // Coalesce: if the newest event is a still-pending resize (no data has
        // been appended since it), replace its geometry in place instead of
        // pushing a new marker. Its `at` is unchanged (no data moved the offset).
        // Bumping the id makes a consumer that already delivered the old marker
        // re-deliver the newer geometry. This bounds resize-only traffic (e.g.
        // repeated pane.set_pty_size with no child output) to one marker.
        match inner.events.back_mut() {
            Some(RingEvent::Resize {
                id: last_id,
                cols: last_cols,
                rows: last_rows,
                ..
            }) => {
                *last_id = id;
                *last_cols = cols;
                *last_rows = rows;
            }
            _ => inner
                .events
                .push_back(RingEvent::Resize { id, at, cols, rows }),
        }
        evict_to_capacity(&mut inner);
        drop(inner);
        self.ready.notify_all();
    }

    /// Capture the current tail offset, resize watermark and geometry.
    ///
    /// Called by `PaneTerminal::output_snapshot` while it holds the core lock, so
    /// the returned offset is a consistent cut with the rendered screen.
    pub(crate) fn capture_offsets(&self) -> (u64, u64, u16, u16) {
        let inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        (
            inner.total_offset,
            inner.resize_counter,
            inner.cols,
            inner.rows,
        )
    }

    /// Block until there is work for the consumer at `cursor`/`resize_id`, the
    /// idle `timeout` elapses, or the ring closes.
    pub(crate) fn wait_for_activity(
        &self,
        cursor: u64,
        resize_id: u64,
        timeout: Duration,
    ) -> OutputWait {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        loop {
            if self.closed.load(Ordering::Acquire) {
                return OutputWait::Closed;
            }
            // All O(1): byte lag, resize lag (missed an evicted resize), new
            // data, or a retained undelivered resize.
            if cursor < inner.head_offset
                || resize_id < inner.resize_floor
                || inner.total_offset > cursor
                || inner.has_retained_pending_resize(resize_id)
            {
                return OutputWait::Ready;
            }
            let (guard, wait_result) = match self.ready.wait_timeout(inner, timeout) {
                Ok(result) => result,
                Err(_) => return OutputWait::Closed,
            };
            inner = guard;
            if wait_result.timed_out() {
                return OutputWait::Idle;
            }
        }
    }

    /// Drain at most one bounded step for a consumer at `cursor`/`resize_id`.
    ///
    /// Returns either a single resize marker (delivered in offset order before
    /// the data that follows it) or up to `max_frame_bytes` of raw byte chunks
    /// as cheap `Bytes` slices that share the ring's buffers. The ring lock is
    /// held only for this slicing — never for base64/concatenation/large
    /// allocation, which the caller does after releasing it — so `append` (run
    /// under the core lock) is never blocked behind slow framing, and a stalled
    /// viewer retains only one frame rather than the whole backlog.
    ///
    /// The lock hold is O(log events + frame): event byte positions are
    /// monotonic, so the consumer's cursor is located by binary search rather
    /// than a front-to-cursor linear scan, then collection walks only the events
    /// that make up one frame.
    pub(crate) fn drain(&self, cursor: u64, resize_id: u64, max_frame_bytes: usize) -> OutputDrain {
        // Callers clamp to a sane minimum at the param boundary; guard only
        // against a zero split size that would not make progress.
        let max_frame_bytes = max_frame_bytes.max(1);
        let inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Lag if the byte cursor fell off the retained window, OR if a resize the
        // consumer still needed was evicted (its id is below the evicted floor).
        // The latter is essential: an evicted resize carries no bytes, so it does
        // not advance `head_offset`, and without this check the consumer would
        // silently render following data with stale geometry.
        if cursor < inner.head_offset || resize_id < inner.resize_floor {
            return OutputDrain::Lagged;
        }

        // Binary search (positions are monotonic) for the first event that is not
        // wholly before the cursor, so we do not linear-scan delivered events.
        let start_idx = cursor_partition_point(&inner.events, cursor);

        let mut chunks: Vec<Bytes> = Vec::new();
        let mut collected = 0usize;
        let mut cur = cursor;
        let mut idx = start_idx;
        while let Some(event) = inner.events.get(idx) {
            match event {
                RingEvent::Data { start, bytes } => {
                    let end = start.saturating_add(bytes.len() as u64);
                    if end <= cur {
                        idx += 1;
                        continue;
                    }
                    let skip = cur.saturating_sub(*start) as usize;
                    if skip >= bytes.len() {
                        idx += 1;
                        continue;
                    }
                    let take = (max_frame_bytes - collected).min(bytes.len() - skip);
                    // `Bytes::slice` is O(1) and shares the buffer: no payload
                    // copy happens under the lock.
                    chunks.push(bytes.slice(skip..skip + take));
                    collected += take;
                    cur = cur.saturating_add(take as u64);
                    if collected >= max_frame_bytes {
                        break;
                    }
                }
                RingEvent::Resize { id, cols, rows, .. } => {
                    if *id <= resize_id {
                        idx += 1;
                        continue;
                    }
                    if chunks.is_empty() {
                        // No pending data precedes this resize: deliver it now.
                        return OutputDrain::Resize {
                            cols: *cols,
                            rows: *rows,
                            resize_id: *id,
                        };
                    }
                    // Flush the data collected so far; the resize is delivered on
                    // the next wake, keeping strict offset order.
                    break;
                }
            }
            idx += 1;
        }
        if chunks.is_empty() {
            return OutputDrain::Idle;
        }
        OutputDrain::Data {
            chunks,
            cursor: cur,
        }
    }

    /// Render a fresh full-screen seed atomically paired with the offset it
    /// reflects, plus up to `history_lines` scrollback rows when non-zero.
    /// Returns `None` if the owning terminal is gone.
    pub(crate) fn snapshot(&self, history_lines: usize) -> Option<OutputSnapshot> {
        let terminal = self.terminal.upgrade()?;
        terminal.output_snapshot(self, history_lines)
    }
}

/// Evict oldest events until the retained data is within the byte capacity and
/// the event count is within `MAX_RING_EVENTS`. Evicting `Data` advances
/// `head_offset` (byte lag); evicting a `Resize` raises `resize_floor` (resize
/// lag) — either forces a resync for a consumer that needed the dropped content.
fn evict_to_capacity(inner: &mut OutputRingInner) {
    while inner.buffered_bytes > inner.capacity || inner.events.len() > MAX_RING_EVENTS {
        match inner.events.pop_front() {
            Some(RingEvent::Data { start, bytes }) => {
                inner.buffered_bytes = inner.buffered_bytes.saturating_sub(bytes.len());
                inner.head_offset = start.saturating_add(bytes.len() as u64);
            }
            Some(RingEvent::Resize { id, .. }) => {
                inner.resize_floor = inner.resize_floor.max(id);
            }
            None => break,
        }
    }
}

/// First index whose event is not wholly before `cursor` (`Data` with
/// `end > cursor`, or a `Resize` at position `>= cursor`). Event positions are
/// monotonically non-decreasing, so this is a binary search over the deque's
/// O(1)-indexable entries — O(log events), avoiding a front-to-cursor scan.
fn cursor_partition_point(events: &VecDeque<RingEvent>, cursor: u64) -> usize {
    let mut lo = 0usize;
    let mut hi = events.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let before = match events.get(mid) {
            Some(RingEvent::Data { start, bytes }) => {
                start.saturating_add(bytes.len() as u64) <= cursor
            }
            Some(RingEvent::Resize { at, .. }) => *at < cursor,
            None => false,
        };
        if before {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Clamp a client-requested frame size into the supported range.
pub(crate) fn clamp_max_frame_bytes(requested: Option<usize>) -> usize {
    requested
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_FRAME_BYTES)
        .max(MIN_MAX_FRAME_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ring(capacity: usize) -> Arc<OutputRing> {
        OutputRing::new(7, capacity, 80, 24, Weak::new())
    }

    /// Concatenate a `Data` step's shared chunks into an owned buffer (what the
    /// server does, outside the ring lock).
    fn data_payload(drain: &OutputDrain) -> Vec<u8> {
        match drain {
            OutputDrain::Data { chunks, .. } => chunks
                .iter()
                .flat_map(|chunk| chunk.iter().copied())
                .collect(),
            _ => panic!("expected a data step"),
        }
    }

    fn data_cursor(drain: &OutputDrain) -> u64 {
        match drain {
            OutputDrain::Data { cursor, .. } => *cursor,
            _ => panic!("expected a data step"),
        }
    }

    #[test]
    fn coalesces_consecutive_appends_into_one_frame() {
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        ring.append(b"abc");
        ring.append(b"def");
        // Both appends coalesce into one bounded step (well under the frame cap).
        let step = ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_cursor(&step), 6);
        assert_eq!(data_payload(&step), b"abcdef");
        // Caught up now.
        assert!(matches!(
            ring.drain(6, 0, DEFAULT_MAX_FRAME_BYTES),
            OutputDrain::Idle
        ));
    }

    #[test]
    fn splits_oversize_delta_by_max_frame_bytes() {
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        ring.append(&[b'x'; 10]);
        // One bounded frame per drain step; the serve loop re-wakes for the rest.
        let mut cursor = 0;
        let mut sizes = Vec::new();
        let mut all = Vec::new();
        loop {
            match ring.drain(cursor, 0, 4) {
                OutputDrain::Data {
                    chunks,
                    cursor: next,
                } => {
                    let payload: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
                    sizes.push(payload.len());
                    all.extend_from_slice(&payload);
                    cursor = next;
                }
                OutputDrain::Idle => break,
                _ => panic!("unexpected drain step"),
            }
        }
        assert_eq!(sizes, vec![4, 4, 2]);
        assert_eq!(all, vec![b'x'; 10]);
    }

    #[test]
    fn offsets_are_monotonic_and_cursor_only_delivers_new_bytes() {
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        ring.append(b"hello");
        let step = ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_cursor(&step), 5);
        ring.append(b" world");
        let step = ring.drain(5, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_cursor(&step), 11);
        assert_eq!(data_payload(&step), b" world");
    }

    #[test]
    fn interleaves_resize_markers_in_offset_order() {
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        ring.append(b"before");
        ring.note_resize(100, 30);
        ring.append(b"after");

        // Step 1: data up to the resize boundary.
        let step = ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_payload(&step), b"before");
        assert_eq!(data_cursor(&step), 6);

        // Step 2: the resize marker (byte cursor unchanged).
        let OutputDrain::Resize {
            cols,
            rows,
            resize_id,
        } = ring.drain(6, 0, DEFAULT_MAX_FRAME_BYTES)
        else {
            panic!("expected resize step");
        };
        assert_eq!((cols, rows, resize_id), (100, 30, 1));

        // Step 3: data after the resize; the resize is not redelivered.
        let step = ring.drain(6, resize_id, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_payload(&step), b"after");
        assert_eq!(data_cursor(&step), 11);
        assert!(matches!(
            ring.drain(11, resize_id, DEFAULT_MAX_FRAME_BYTES),
            OutputDrain::Idle
        ));
    }

    #[test]
    fn overflow_evicts_backlog_and_reports_lag_for_stale_cursor() {
        // Tiny capacity so a second append forces eviction of the first.
        let ring = test_ring(8);
        ring.append(&[b'a'; 8]);
        // A cursor at 0 is still in range here.
        assert!(matches!(
            ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES),
            OutputDrain::Data { .. }
        ));
        // This append pushes buffered bytes over capacity and evicts the first.
        ring.append(&[b'b'; 8]);
        // A consumer still parked at offset 0 has fallen off the window.
        assert!(matches!(
            ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES),
            OutputDrain::Lagged
        ));
        // A consumer at the live edge (offset 8) still gets the fresh bytes.
        let step = ring.drain(8, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_cursor(&step), 16);
        assert_eq!(data_payload(&step), vec![b'b'; 8]);
    }

    #[test]
    fn resize_only_traffic_stays_bounded() {
        // Repeated resizes with no child output must not grow the ring: they
        // coalesce to a single trailing marker.
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        for i in 0..10_000u16 {
            ring.note_resize(80 + (i % 40), 24 + (i % 10));
        }
        {
            let inner = ring.inner.lock().unwrap();
            assert_eq!(
                inner.events.len(),
                1,
                "resize-only traffic must stay bounded"
            );
        }
        // A fresh consumer still gets the latest coalesced geometry, exactly once.
        let OutputDrain::Resize {
            cols,
            rows,
            resize_id,
        } = ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES)
        else {
            panic!("expected the coalesced resize");
        };
        assert_eq!((cols, rows), (80 + (9999 % 40), 24 + (9999 % 10)));
        assert_eq!(resize_id, 10_000);
        assert!(matches!(
            ring.drain(0, resize_id, DEFAULT_MAX_FRAME_BYTES),
            OutputDrain::Idle
        ));
    }

    #[test]
    fn interleaved_resizes_are_retained_but_coalesce_when_trailing() {
        // A resize with data after it is kept; a run of resizes with no data
        // between them collapses to one.
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        ring.append(b"a");
        ring.note_resize(90, 25);
        ring.append(b"b");
        ring.note_resize(100, 26);
        ring.note_resize(110, 27);
        ring.note_resize(120, 28);
        let inner = ring.inner.lock().unwrap();
        // Data("a"), Resize(90,25), Data("b"), Resize(coalesced 120,28) = 4 events.
        assert_eq!(inner.events.len(), 4);
    }

    #[test]
    fn evicted_resize_forces_resync_not_stale_geometry() {
        // A leading resize evicted by the event-count cap carries no bytes, so
        // `head_offset` does not advance. A consumer that still needed it must be
        // told to resync (never handed the following data under stale geometry).
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        // One resize at offset 0, then enough 1-byte appends to overflow the
        // event ceiling and evict that leading resize (bytes stay well under
        // capacity, so this is purely the event-count path — head stays at 0).
        ring.note_resize(120, 40);
        for _ in 0..MAX_RING_EVENTS {
            ring.append(b"x");
        }
        {
            let inner = ring.inner.lock().unwrap();
            assert_eq!(inner.head_offset, 0, "byte window did not move");
            assert_eq!(inner.resize_floor, 1, "the evicted resize raised the floor");
            assert!(inner
                .events
                .iter()
                .all(|event| matches!(event, RingEvent::Data { .. })));
        }
        // The consumer sat at cursor 0 having delivered no resize (resize_id 0).
        // Its byte cursor is still in range (head is 0), but the resize it needed
        // was evicted, so it must be told to resync rather than get data at 0.
        assert!(
            matches!(
                ring.drain(0, 0, DEFAULT_MAX_FRAME_BYTES),
                OutputDrain::Lagged
            ),
            "an evicted required resize must force a resync"
        );
        // wait_for_activity must also wake it (so the resync actually happens).
        assert!(matches!(
            ring.wait_for_activity(0, 0, Duration::from_millis(10)),
            OutputWait::Ready
        ));
        // A consumer that had already delivered that resize (resize_id 1) is fine
        // and simply receives the buffered data.
        assert!(matches!(
            ring.drain(0, 1, DEFAULT_MAX_FRAME_BYTES),
            OutputDrain::Data { .. }
        ));
    }

    #[test]
    fn binary_search_locates_cursor_across_many_chunks() {
        // Many chunks so a front-to-cursor scan would be expensive; drain must
        // still return exactly the bytes at an arbitrary interior cursor.
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        for i in 0..2_000u32 {
            ring.append(format!("[{i:04}]").as_bytes()); // 6 bytes each
        }
        // Cursor near the end, mid-way through a chunk boundary region.
        let cursor = 6 * 1_999; // start of the final "[1999]" chunk
        let step = ring.drain(cursor, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_cursor(&step), 6 * 2_000);
        assert_eq!(data_payload(&step), b"[1999]");

        // A mid-chunk cursor is honored too (skip within the located chunk).
        let step = ring.drain(cursor + 2, 0, DEFAULT_MAX_FRAME_BYTES);
        assert_eq!(data_payload(&step), b"999]");
    }

    #[test]
    fn no_subscribers_leaves_ring_untouched_and_offset_monotonic() {
        // The zero-viewer fast path lives in `process_pty_bytes` (it never calls
        // `append` when no ring is attached); here we assert the ring's own
        // monotonic-offset contract that the fast path relies on.
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        let (before, _, _, _) = ring.capture_offsets();
        assert_eq!(before, 0);
        ring.append(b"x");
        let (after, _, _, _) = ring.capture_offsets();
        assert_eq!(after, 1);
    }

    #[test]
    fn mark_closed_wakes_waiters_with_closed() {
        let ring = test_ring(OUTPUT_RING_CAPACITY_BYTES);
        assert!(!ring.closed.load(Ordering::Acquire));
        ring.mark_closed();
        assert!(ring.closed.load(Ordering::Acquire));
        assert!(matches!(
            ring.wait_for_activity(0, 0, Duration::from_millis(10)),
            OutputWait::Closed
        ));
    }
}
