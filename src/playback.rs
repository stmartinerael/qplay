//! Playback calculations independent of input, windowing, and media backends.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Commands handled entirely by playback policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Select(usize),
    TogglePause,
    ToggleSelectedPause,
    SetSelectedPause(bool),
}

/// Playback decisions, independent of video decoding and windowing.
#[derive(Debug, Default)]
pub struct PlaybackCore {
    selected: usize,
    paused: bool,
    slots_paused: [bool; 4],
    slots: [Option<SlotSnapshot>; 4],
    identities: [Option<PathBuf>; 4],
    requests: [u64; 4],
}

/// Read-only metadata for the frame currently displayed by a slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotSnapshot {
    pub fps: f64,
    pub frame_count: i32,
    pub position: i32,
    pub clip_start: i32,
    /// Exclusive clip end.
    pub clip_end: Option<i32>,
    pub next_frame_at: Instant,
    pub transition_started_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    Wait,
    Read,
    Replace,
}

impl PlaybackCore {
    pub fn apply(&mut self, command: Command) -> Result<(), &'static str> {
        match command {
            Command::Select(index) if index < self.slots_paused.len() => self.selected = index,
            Command::Select(_) => return Err("invalid slot index"),
            Command::TogglePause => self.paused = !self.paused,
            Command::ToggleSelectedPause => {
                self.slots_paused[self.selected] = !self.slots_paused[self.selected]
            }
            Command::SetSelectedPause(paused) => self.slots_paused[self.selected] = paused,
        }
        Ok(())
    }
    pub fn slot(&self, index: usize) -> Option<SlotSnapshot> {
        self.slots.get(index).copied().flatten()
    }

    /// Commit metadata only after a backend has opened and decoded the first frame.
    pub fn opened(
        &mut self,
        index: usize,
        fps: f64,
        frame_count: i32,
        clip: (i32, Option<i32>),
        now: Instant,
        identity: PathBuf,
    ) -> Result<(), &'static str> {
        let (start, end) = clip;
        if index >= self.slots.len()
            || !fps.is_finite()
            || fps <= 0.0
            || frame_count < 0
            || start < 0
            || (frame_count > 0 && start >= frame_count)
            || end.is_some_and(|end| end <= start)
        {
            return Err("invalid slot metadata");
        }
        self.identities[index] = Some(identity);
        self.slots[index] = Some(SlotSnapshot {
            fps,
            frame_count,
            position: start,
            clip_start: start,
            clip_end: end,
            next_frame_at: next_frame_time(now, None, fps),
            transition_started_at: None,
        });
        Ok(())
    }
    /// Invalidate earlier asynchronous requests for this slot.
    pub fn new_request(&mut self, index: usize) -> Option<u64> {
        let id = self.requests.get_mut(index)?;
        *id = id.checked_add(1).expect("request identity exhausted");
        Some(*id)
    }
    pub fn request_is_current(&self, index: usize, id: u64) -> bool {
        self.requests
            .get(index)
            .is_some_and(|current| *current == id)
    }
    pub fn identity(&self, index: usize) -> Option<&Path> {
        self.identities.get(index).and_then(Option::as_deref)
    }
    pub fn start_transition(&mut self, index: usize, now: Instant) {
        if let Some(Some(slot)) = self.slots.get_mut(index) {
            slot.transition_started_at = Some(now);
        }
    }
    pub fn clear(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = None;
            self.identities[index] = None;
        }
    }
    pub fn resume_at(&mut self, now: Instant) {
        for slot in self.slots.iter_mut().flatten() {
            slot.next_frame_at = now;
        }
    }
    pub fn seek_target(&self, index: usize, seconds: f64) -> Option<i32> {
        self.slot(index)
            .map(|slot| seek_target(slot.position, seconds, slot.fps, slot.frame_count))
    }
    /// Commit a seek only after the backend successfully decodes the target.
    pub fn seeked(&mut self, index: usize, target: i32, now: Instant) {
        if let Some(Some(slot)) = self.slots.get_mut(index) {
            slot.position = target;
            slot.next_frame_at = now;
        }
    }
    pub fn frame_action(&self, index: usize, now: Instant) -> FrameAction {
        let Some(slot) = self.slot(index) else {
            return FrameAction::Replace;
        };
        if self.slot_paused(index).unwrap_or(true) || now < slot.next_frame_at {
            return FrameAction::Wait;
        }
        if slot
            .clip_end
            .is_some_and(|end| slot.position.saturating_add(1) >= end)
        {
            FrameAction::Replace
        } else {
            FrameAction::Read
        }
    }
    pub fn frame_read(&mut self, index: usize, now: Instant) {
        if let Some(Some(slot)) = self.slots.get_mut(index) {
            slot.position = slot.position.saturating_add(1);
            slot.next_frame_at = next_frame_time(now, Some(slot.next_frame_at), slot.fps);
        }
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn paused(&self) -> bool {
        self.paused
    }
    pub fn individually_paused(&self, index: usize) -> Option<bool> {
        self.slots_paused.get(index).copied()
    }
    pub fn slot_paused(&self, index: usize) -> Option<bool> {
        self.slots_paused
            .get(index)
            .map(|paused| self.paused || *paused)
    }
}

/// Fade progress for an explicit time, clamped to the transition interval.
pub fn transition_progress(started: Instant, now: Instant, duration: Duration) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    (now.saturating_duration_since(started).as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
}

/// Seek to a nonnegative frame, bounded by the last frame when the count is known.
pub fn seek_target(position: i32, seconds: f64, fps: f64, frame_count: i32) -> i32 {
    let offset = (seconds * fps.max(1.0)) as i32;
    let target = position.saturating_add(offset).max(0);
    if frame_count > 0 {
        target.min(frame_count - 1)
    } else {
        target
    }
}

/// Schedule the next frame without trying to catch up after a stall.
pub fn next_frame_time(now: Instant, previous: Option<Instant>, fps: f64) -> Instant {
    let fps = if fps.is_finite() && fps > 1.0 {
        fps
    } else {
        30.0
    };
    previous.unwrap_or(now).max(now) + Duration::from_secs_f64(1.0 / fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_preserve_slot_pause_across_global_pause() {
        let mut core = PlaybackCore::default();
        core.apply(Command::Select(2)).unwrap();
        core.apply(Command::ToggleSelectedPause).unwrap();
        core.apply(Command::TogglePause).unwrap();
        assert_eq!(core.slot_paused(0), Some(true));
        core.apply(Command::TogglePause).unwrap();
        assert_eq!(core.slot_paused(0), Some(false));
        assert_eq!(core.slot_paused(2), Some(true));
        assert!(core.apply(Command::Select(4)).is_err());
        assert_eq!(core.selected(), 2);
    }

    #[test]
    fn request_id_rejects_stale_completions_per_slot() {
        let mut core = PlaybackCore::default();
        let first = core.new_request(0).unwrap();
        let other = core.new_request(1).unwrap();
        let second = core.new_request(0).unwrap();
        assert!(!core.request_is_current(0, first));
        assert!(core.request_is_current(0, second));
        assert!(core.request_is_current(1, other));
        assert!(!core.request_is_current(4, second));
    }

    #[test]
    fn seek_respects_known_and_unknown_bounds() {
        assert_eq!(seek_target(10, -5.0, 30.0, 100), 0);
        assert_eq!(seek_target(10, 5.0, 30.0, 100), 99);
        assert_eq!(seek_target(10, 5.0, 30.0, 0), 160);
    }

    #[test]
    fn timeline_commits_only_successful_operations_and_honors_boundaries() {
        let now = Instant::now();
        let mut core = PlaybackCore::default();
        assert_eq!(core.frame_action(0, now), FrameAction::Replace);
        core.opened(0, 10.0, 20, (4, Some(6)), now, "one".into())
            .unwrap();
        assert_eq!(core.slot(0).unwrap().position, 4);
        let original = core.slot(0);
        assert!(
            core.opened(0, f64::NAN, 20, (0, None), now, "bad".into())
                .is_err()
        );
        assert_eq!(core.slot(0), original);
        assert_eq!(core.identity(0), Some(Path::new("one")));
        assert_eq!(core.frame_action(0, now), FrameAction::Wait);
        let first_due = now + Duration::from_millis(100);
        assert_eq!(core.frame_action(0, first_due), FrameAction::Read);
        core.frame_read(0, first_due);
        assert_eq!(core.slot(0).unwrap().position, 5);
        assert_eq!(core.frame_action(0, now), FrameAction::Wait);
        assert_eq!(
            core.frame_action(0, now + Duration::from_millis(200)),
            FrameAction::Replace
        );
        core.apply(Command::TogglePause).unwrap();
        assert_eq!(
            core.frame_action(0, now + Duration::from_secs(1)),
            FrameAction::Wait
        );
        let target = core.seek_target(0, -1.0).unwrap();
        assert_eq!(target, 0);
        assert_eq!(core.slot(0).unwrap().position, 5); // a failed seek never commits
        core.seeked(0, target, now);
        assert_eq!(core.slot(0).unwrap().position, 0);
    }

    #[test]
    fn transition_progress_is_bounded_and_uses_supplied_time() {
        let start = Instant::now();
        assert_eq!(
            transition_progress(start, start, Duration::from_millis(200)),
            0.0
        );
        assert_eq!(
            transition_progress(
                start,
                start + Duration::from_millis(100),
                Duration::from_millis(200)
            ),
            0.5
        );
        assert_eq!(
            transition_progress(
                start,
                start + Duration::from_secs(1),
                Duration::from_millis(200)
            ),
            1.0
        );
        assert_eq!(transition_progress(start, start, Duration::ZERO), 1.0);
    }

    #[test]
    fn scheduler_does_not_accumulate_lag() {
        let now = Instant::now();
        assert_eq!(
            next_frame_time(now, Some(now - Duration::from_secs(1)), 20.0),
            now + Duration::from_millis(50)
        );
        assert_eq!(
            next_frame_time(now, None, 0.0),
            now + Duration::from_secs_f64(1.0 / 30.0)
        );
    }
}
