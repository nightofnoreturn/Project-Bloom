//! Pure dynamic segment planner implementing work-stealing multi-connection partitioning
//! for Samsung FUS firmware downloads (zero network/disk I/O).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Split boundaries align to 4 KiB pages for clean random-access writes on NTFS/ext4/APFS.
pub const SEGMENT_ALIGN: u64 = 4096;

/// Smallest remaining half allowed when splitting an active segment (2 MiB).
pub const DEFAULT_MIN_SPLIT: u64 = 2 * 1024 * 1024;

/// Default grace period before a free worker preempts another worker that has not yet
/// received its first byte from the server.
pub const DEFAULT_PREEMPT_AFTER: Duration = Duration::from_secs(6);

/// Default grace period before a free worker preempts a stalled mid-stream connection.
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(18);

/// Persisted segment state inside `.bloom-state.json` (inclusive `end`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentState {
    pub id: usize,
    pub start: u64,
    /// Inclusive end byte offset.
    pub end: u64,
    pub downloaded: u64,
}

impl SegmentState {
    pub fn total_len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded >= self.total_len()
    }

    pub fn next_offset(&self) -> u64 {
        self.start + self.downloaded
    }
}

/// UI/observer-friendly progress snapshot for one segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentProgress {
    pub start: u64,
    pub downloaded: u64,
    /// Exclusive end byte offset.
    pub end: u64,
    pub active: bool,
}

/// Half-open `[from, to)` byte range assigned to a download worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentAssignment {
    pub from: u64,
    pub to: u64,
}

/// Reservation outcome returned by [`SegmentPlanner::claim`] before disk writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimResult {
    /// Worker may write up to this many bytes at the claimed offset (clamped if split).
    Granted(u64),
    /// Segment has reached its (possibly shrunken) boundary cleanly.
    Finished,
    /// Segment ownership was reassigned or preempted; stop writing immediately.
    Lost,
}

#[derive(Debug, Clone)]
struct LiveSegment {
    start: u64,
    cursor: u64,
    reserved: u64,
    end: u64,
    worker: Option<u32>,
    receiving: bool,
    assigned_at: Option<Instant>,
    last_progress_at: Option<Instant>,
}

impl LiveSegment {
    fn unclaimed(&self) -> u64 {
        self.end.saturating_sub(self.reserved)
    }

    fn remaining_uncommitted(&self) -> u64 {
        self.end.saturating_sub(self.cursor)
    }

    fn is_complete(&self) -> bool {
        self.cursor >= self.end
    }
}

/// Coordinates dynamic segment splitting, work stealing, and stall preemption across workers.
#[derive(Debug, Clone)]
pub struct SegmentPlanner {
    total_size: u64,
    min_split: u64,
    preempt_after: Duration,
    stall_timeout: Duration,
    segs: Vec<LiveSegment>,
}

impl SegmentPlanner {
    /// Creates a fresh planner with a single `[0, total_size)` segment ready to split dynamically.
    pub fn new(total_size: u64, min_split: u64) -> Self {
        let segs = if total_size == 0 {
            Vec::new()
        } else {
            vec![LiveSegment {
                start: 0,
                cursor: 0,
                reserved: 0,
                end: total_size,
                worker: None,
                receiving: false,
                assigned_at: None,
                last_progress_at: None,
            }]
        };
        Self {
            total_size,
            min_split: min_split.max(SEGMENT_ALIGN),
            preempt_after: DEFAULT_PREEMPT_AFTER,
            stall_timeout: DEFAULT_STALL_TIMEOUT,
            segs,
        }
    }

    /// Reconstructs planner state from `.bloom-state.json` if `saved` tiles `[0, total_size)`.
    pub fn from_saved(total_size: u64, min_split: u64, saved: &[SegmentState]) -> Option<Self> {
        if total_size == 0 {
            return saved.is_empty().then(|| Self::new(0, min_split));
        }
        if saved.is_empty() {
            return None;
        }

        let mut expect = 0u64;
        let mut segs = Vec::with_capacity(saved.len());
        for s in saved {
            if s.start != expect || s.end < s.start {
                return None;
            }
            let exclusive_end = s.end.checked_add(1)?;
            let len = exclusive_end - s.start;
            let downloaded = s.downloaded.min(len);
            let cursor = s.start + downloaded;
            segs.push(LiveSegment {
                start: s.start,
                cursor,
                reserved: cursor,
                end: exclusive_end,
                worker: None,
                receiving: false,
                assigned_at: None,
                last_progress_at: None,
            });
            expect = exclusive_end;
        }

        if expect != total_size {
            return None;
        }

        Some(Self {
            total_size,
            min_split: min_split.max(SEGMENT_ALIGN),
            preempt_after: DEFAULT_PREEMPT_AFTER,
            stall_timeout: DEFAULT_STALL_TIMEOUT,
            segs,
        })
    }

    pub fn set_preempt_after(&mut self, duration: Duration) {
        self.preempt_after = duration;
    }

    pub fn set_stall_timeout(&mut self, duration: Duration) {
        self.stall_timeout = duration;
    }

    /// Exports current committed offsets in `.bloom-state.json` format.
    pub fn to_saved(&self) -> Vec<SegmentState> {
        self.segs
            .iter()
            .enumerate()
            .map(|(id, seg)| SegmentState {
                id,
                start: seg.start,
                end: seg.end - 1,
                downloaded: seg.cursor.saturating_sub(seg.start),
            })
            .collect()
    }

    pub fn snapshot(&self) -> Vec<SegmentProgress> {
        self.segs
            .iter()
            .map(|seg| SegmentProgress {
                start: seg.start,
                downloaded: seg.cursor.saturating_sub(seg.start),
                end: seg.end,
                active: seg.worker.is_some() && !seg.is_complete(),
            })
            .collect()
    }

    pub fn downloaded_bytes(&self) -> u64 {
        self.segs
            .iter()
            .map(|seg| seg.cursor.saturating_sub(seg.start))
            .sum()
    }

    pub fn is_complete(&self) -> bool {
        self.segs.iter().all(LiveSegment::is_complete)
    }

    /// Assigns work to `worker_id`:
    /// 1. Picks up any unassigned incomplete segment (e.g. after resume or error).
    /// 2. Else preempts a stalled / non-receiving active worker.
    /// 3. Else splits the largest remaining unclaimed tail in half (4 KiB aligned).
    pub fn assign(&mut self, worker_id: u32, now: Instant) -> Option<SegmentAssignment> {
        self.release(worker_id);

        // 1. First unassigned incomplete segment
        if let Some(seg) = self
            .segs
            .iter_mut()
            .find(|s| s.worker.is_none() && !s.is_complete())
        {
            seg.reserved = seg.cursor;
            seg.worker = Some(worker_id);
            seg.receiving = false;
            seg.assigned_at = Some(now);
            seg.last_progress_at = None;
            return Some(SegmentAssignment {
                from: seg.cursor,
                to: seg.end,
            });
        }

        // 2. Preempt slow-start or mid-stream stalled worker with the largest remaining tail
        let stalled_idx = self
            .segs
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.worker.is_some()
                    && !s.is_complete()
                    && ((!s.receiving
                        && s.assigned_at
                            .is_some_and(|t| now.saturating_duration_since(t) >= self.preempt_after))
                        || (s.receiving
                            && s.last_progress_at
                                .is_some_and(|t| now.saturating_duration_since(t) >= self.stall_timeout)))
            })
            .max_by_key(|(_, s)| s.remaining_uncommitted())
            .map(|(idx, _)| idx);

        if let Some(idx) = stalled_idx {
            let seg = &mut self.segs[idx];
            seg.reserved = seg.cursor;
            seg.worker = Some(worker_id);
            seg.receiving = false;
            seg.assigned_at = Some(now);
            seg.last_progress_at = None;
            return Some(SegmentAssignment {
                from: seg.cursor,
                to: seg.end,
            });
        }

        // 3. Split the segment with the largest unclaimed tail in half
        let split_candidate = self
            .segs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.unclaimed() >= self.min_split.saturating_mul(2))
            .max_by_key(|(_, s)| s.unclaimed())
            .map(|(idx, _)| idx);

        let idx = split_candidate?;
        let (reserved, old_end, unclaimed) = {
            let seg = &self.segs[idx];
            (seg.reserved, seg.end, seg.unclaimed())
        };

        let raw_mid = reserved + unclaimed / 2;
        let aligned_down = raw_mid & !(SEGMENT_ALIGN - 1);
        let aligned_up = (raw_mid.saturating_add(SEGMENT_ALIGN - 1)) & !(SEGMENT_ALIGN - 1);

        let split_at = if aligned_down >= reserved + self.min_split
            && old_end.saturating_sub(aligned_down) >= self.min_split
        {
            aligned_down
        } else if aligned_up >= reserved + self.min_split
            && old_end.saturating_sub(aligned_up) >= self.min_split
        {
            aligned_up
        } else {
            raw_mid
        };

        if split_at <= reserved || split_at >= old_end {
            return None;
        }

        self.segs[idx].end = split_at;
        let new_seg = LiveSegment {
            start: split_at,
            cursor: split_at,
            reserved: split_at,
            end: old_end,
            worker: Some(worker_id),
            receiving: false,
            assigned_at: Some(now),
            last_progress_at: None,
        };
        self.segs.insert(idx + 1, new_seg);

        Some(SegmentAssignment {
            from: split_at,
            to: old_end,
        })
    }

    /// Reserves up to `wanted` bytes at `offset` for `worker_id` before writing to disk.
    pub fn claim(&mut self, worker_id: u32, offset: u64, wanted: u64, now: Instant) -> ClaimResult {
        let Some(seg) = self
            .segs
            .iter_mut()
            .find(|s| s.worker == Some(worker_id) && s.reserved == offset)
        else {
            return ClaimResult::Lost;
        };

        let avail = seg.end.saturating_sub(seg.reserved);
        if avail == 0 {
            seg.worker = None;
            seg.receiving = false;
            seg.assigned_at = None;
            seg.last_progress_at = None;
            return ClaimResult::Finished;
        }

        let granted = wanted.min(avail);
        seg.reserved += granted;
        seg.receiving = true;
        seg.last_progress_at = Some(now);
        ClaimResult::Granted(granted)
    }

    /// Commits `len` written bytes starting at `offset` for `worker_id`.
    pub fn commit(&mut self, worker_id: u32, offset: u64, len: u64) -> bool {
        let Some(seg) = self
            .segs
            .iter_mut()
            .find(|s| s.worker == Some(worker_id) && s.cursor == offset)
        else {
            return false;
        };

        seg.cursor = (seg.cursor + len).min(seg.end);
        if seg.reserved < seg.cursor {
            seg.reserved = seg.cursor;
        }
        if seg.cursor >= seg.end {
            seg.worker = None;
            seg.receiving = false;
            seg.assigned_at = None;
            seg.last_progress_at = None;
        }
        true
    }

    /// Releases any active segment owned by `worker_id` so another worker can resume it cleanly.
    pub fn release(&mut self, worker_id: u32) {
        for seg in &mut self.segs {
            if seg.worker == Some(worker_id) {
                seg.reserved = seg.cursor;
                seg.worker = None;
                seg.receiving = false;
                seg.assigned_at = None;
                seg.last_progress_at = None;
            }
        }
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_largest_tail_and_prevents_overlap() {
        let t0 = Instant::now();
        let total = 20 * 1024 * 1024;
        let min_split = 2 * 1024 * 1024;
        let mut planner = SegmentPlanner::new(total, min_split);

        let a0 = planner.assign(0, t0).expect("worker 0 assignment");
        assert_eq!(a0, SegmentAssignment { from: 0, to: total });

        // Worker 0 receives 2 MiB
        let chunk = 2 * 1024 * 1024;
        assert_eq!(
            planner.claim(0, 0, chunk, t0),
            ClaimResult::Granted(chunk)
        );
        assert!(planner.commit(0, 0, chunk));

        // Worker 1 joins and splits remaining [2 MiB, 20 MiB) in half at 11 MiB (aligned to 4 KiB)
        let a1 = planner.assign(1, t0).expect("worker 1 split assignment");
        assert_eq!(a1.from % SEGMENT_ALIGN, 0);
        assert_eq!(a1.from, 11 * 1024 * 1024);
        assert_eq!(a1.to, total);

        // Worker 0 can only claim up to 11 MiB, then finishes cleanly without overlapping worker 1
        let rem0 = 9 * 1024 * 1024;
        assert_eq!(
            planner.claim(0, chunk, rem0 + 4096, t0),
            ClaimResult::Granted(rem0)
        );
        assert!(planner.commit(0, chunk, rem0));
        assert_eq!(planner.claim(0, a1.from, 1024, t0), ClaimResult::Lost);
        assert_eq!(planner.downloaded_bytes(), rem0 + chunk);
    }

    #[test]
    fn preempts_non_receiving_and_stalled_workers() {
        let t0 = Instant::now();
        let total = 3 * 1024 * 1024;
        let mut planner = SegmentPlanner::new(total, 2 * 1024 * 1024);
        planner.set_preempt_after(Duration::from_secs(5));
        planner.set_stall_timeout(Duration::from_secs(10));

        assert!(planner.assign(0, t0).is_some());

        // Too small to split (< 4 MiB) and hasn't timed out yet -> worker 1 waits
        assert!(planner.assign(1, t0 + Duration::from_secs(2)).is_none());

        // After preempt_after without first byte, worker 1 takes over from worker 0
        let takeover = planner
            .assign(1, t0 + Duration::from_secs(6))
            .expect("preempt slow start");
        assert_eq!(takeover, SegmentAssignment { from: 0, to: total });
        assert_eq!(
            planner.claim(0, 0, 1024, t0 + Duration::from_secs(6)),
            ClaimResult::Lost
        );

        // Worker 1 receives some bytes then stalls
        assert_eq!(
            planner.claim(1, 0, 4096, t0 + Duration::from_secs(7)),
            ClaimResult::Granted(4096)
        );
        assert!(planner.commit(1, 0, 4096));

        let rescue = planner
            .assign(2, t0 + Duration::from_secs(18))
            .expect("preempt stalled stream");
        assert_eq!(rescue, SegmentAssignment { from: 4096, to: total });
        assert_eq!(
            planner.claim(1, 4096, 4096, t0 + Duration::from_secs(18)),
            ClaimResult::Lost
        );
    }

    #[test]
    fn saved_state_roundtrips_without_gaps() {
        let t0 = Instant::now();
        let total = 16 * 1024 * 1024;
        let mut planner = SegmentPlanner::new(total, 2 * 1024 * 1024);
        let _ = planner.assign(0, t0).unwrap();
        assert_eq!(planner.claim(0, 0, 4096, t0), ClaimResult::Granted(4096));
        assert!(planner.commit(0, 0, 4096));
        let _ = planner.assign(1, t0).unwrap();

        let saved = planner.to_saved();
        let restored =
            SegmentPlanner::from_saved(total, 2 * 1024 * 1024, &saved).expect("valid saved state");
        assert_eq!(restored.downloaded_bytes(), 4096);
        assert_eq!(restored.to_saved(), saved);
        assert!(!restored.is_complete());
    }
}
