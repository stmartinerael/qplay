//! Playback calculations independent of input, windowing, and media backends.
use crate::video::{VideoEntry, VideoPool};
use rand::{Rng, SeedableRng};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Commands handled entirely by playback policy.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Select(usize),
    TogglePause,
    ToggleSelectedPause,
    SetSelectedPause(bool),
    Tick {
        slot: usize,
        now: Instant,
    },
    Seek {
        slot: usize,
        seconds: f64,
        now: Instant,
    },
    Replace {
        slot: usize,
        path: PathBuf,
        now: Instant,
    },
    EndOfMedia {
        slot: usize,
        now: Instant,
    },
    ToggleTransition,
}

/// Operations for the capture coordinator; ids are scoped to the slot.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    Replace {
        slot: usize,
    },
    Open {
        slot: usize,
        id: u64,
        path: PathBuf,
    },
    Seek {
        slot: usize,
        id: u64,
        target: i32,
        old_position: i32,
    },
    Read {
        slot: usize,
        id: u64,
    },
}
type OpenMetadata = (f64, i32, (i32, Option<i32>), PathBuf);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Open(u64),
    Seek(u64, i32),
    Read(u64),
}

/// Playback decisions, independent of video decoding and windowing.
pub struct PlaybackCore {
    pool: VideoPool,
    rng: rand::rngs::StdRng,
    selected: usize,
    paused: bool,
    slots_paused: [bool; 4],
    slots: [Option<SlotSnapshot>; 4],
    identities: [Option<PathBuf>; 4],
    requests: [u64; 4],
    operations: [u64; 4],
    pending: [Option<Pending>; 4],
    fade: bool,
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

impl Default for PlaybackCore {
    fn default() -> Self {
        Self::new(Vec::new(), false, None)
    }
}

impl PlaybackCore {
    pub fn new(entries: Vec<VideoEntry>, shuffle: bool, seed: Option<u64>) -> Self {
        Self {
            pool: VideoPool::new(entries, shuffle, seed),
            rng: seed.map_or_else(
                rand::rngs::StdRng::from_entropy,
                rand::rngs::StdRng::seed_from_u64,
            ),
            selected: 0,
            paused: false,
            slots_paused: [false; 4],
            slots: [None; 4],
            identities: Default::default(),
            requests: [0; 4],
            operations: [0; 4],
            pending: [None; 4],
            fade: true,
        }
    }
    pub fn reshuffle(&mut self) {
        self.pool.reshuffle();
    }
    pub fn pool_size(&self) -> usize {
        self.pool.total_entries()
    }
    pub fn next_entry(&mut self, index: usize) -> Option<VideoEntry> {
        if index >= self.slots.len() {
            return None;
        }
        let exclude: Vec<_> = self
            .identities
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .filter_map(|(_, path)| path.clone())
            .collect();
        self.pool.next_entry(&exclude)
    }
    pub fn clip_bounds(&mut self, seconds: f64, fps: f64, frame_count: i32) -> (i32, Option<i32>) {
        if seconds <= 0.0 {
            return (0, None);
        }
        let frames = ((seconds * fps).round() as i32).max(1);
        let start = if frame_count > frames {
            self.rng.gen_range(0..=frame_count - frames)
        } else {
            0
        };
        (start, Some(start.saturating_add(frames)))
    }
    fn operation(&mut self, slot: usize) -> Result<u64, &'static str> {
        let id = self.operations.get_mut(slot).ok_or("invalid slot index")?;
        *id = id.checked_add(1).ok_or("operation identity exhausted")?;
        Ok(*id)
    }
    pub fn set_fade(&mut self, fade: bool) {
        self.fade = fade;
    }
    pub fn fade(&self) -> bool {
        self.fade
    }
    pub fn apply(&mut self, command: Command) -> Result<Effect, &'static str> {
        match command {
            Command::Select(index) if index < self.slots_paused.len() => self.selected = index,
            Command::Select(_) => return Err("invalid slot index"),
            Command::TogglePause => self.paused = !self.paused,
            Command::ToggleSelectedPause => {
                self.slots_paused[self.selected] = !self.slots_paused[self.selected]
            }
            Command::SetSelectedPause(paused) => self.slots_paused[self.selected] = paused,
            Command::ToggleTransition => self.fade = !self.fade,
            Command::Replace { slot, path, now: _ } => {
                let id = self.operation(slot)?;
                self.pending[slot] = Some(Pending::Open(id));
                return Ok(Effect::Open { slot, id, path });
            }
            Command::Seek {
                slot,
                seconds,
                now: _,
            } => {
                if slot >= self.slots.len() || !seconds.is_finite() {
                    return Err("invalid seek");
                }
                let Some(snapshot) = self.slot(slot) else {
                    return Ok(Effect::None);
                };
                let target = seek_target(
                    snapshot.position,
                    seconds,
                    snapshot.fps,
                    snapshot.frame_count,
                );
                let id = self.operation(slot)?;
                self.pending[slot] = Some(Pending::Seek(id, target));
                return Ok(Effect::Seek {
                    slot,
                    id,
                    target,
                    old_position: snapshot.position,
                });
            }
            Command::Tick { slot, now } => {
                if slot >= self.slots.len() {
                    return Err("invalid slot index");
                }
                match self.frame_action(slot, now) {
                    FrameAction::Wait => {}
                    FrameAction::Replace => return Ok(Effect::Replace { slot }),
                    FrameAction::Read => {
                        let id = self.operation(slot)?;
                        self.pending[slot] = Some(Pending::Read(id));
                        return Ok(Effect::Read { slot, id });
                    }
                }
            }
            Command::EndOfMedia { slot, now: _ } => {
                if slot >= self.slots.len() {
                    return Err("invalid slot index");
                }
                return Ok(Effect::Replace { slot });
            }
        }
        Ok(Effect::None)
    }
    pub fn open_result(
        &mut self,
        slot: usize,
        id: u64,
        result: Option<OpenMetadata>,
        now: Instant,
    ) -> Result<bool, &'static str> {
        if self.pending.get(slot).copied().flatten() != Some(Pending::Open(id)) {
            return Ok(false);
        }
        self.pending[slot] = None;
        if let Some((fps, count, clip, path)) = result {
            self.opened(slot, fps, count, clip, now, path)?;
            return Ok(true);
        }
        Ok(false)
    }
    pub fn seek_result(&mut self, slot: usize, id: u64, success: bool, now: Instant) -> bool {
        let Some(Some(Pending::Seek(pending_id, target))) = self.pending.get(slot).copied() else {
            return false;
        };
        if pending_id != id {
            return false;
        }
        self.pending[slot] = None;
        if success {
            self.seeked(slot, target, now);
        }
        success
    }
    pub fn read_result(&mut self, slot: usize, id: u64, success: bool, now: Instant) -> bool {
        if self.pending.get(slot).copied().flatten() != Some(Pending::Read(id)) {
            return false;
        }
        self.pending[slot] = None;
        if success {
            self.frame_read(slot, now);
        }
        success
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
            self.pending[index] = None;
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
    fn seeded_selection_excludes_other_slots_and_clip_bounds_are_reproducible() {
        let entries = vec![VideoEntry::new("one".into()), VideoEntry::new("two".into())];
        let mut a = PlaybackCore::new(entries.clone(), true, Some(42));
        let mut b = PlaybackCore::new(entries, true, Some(42));
        let now = Instant::now();
        let first = a.next_entry(0).unwrap();
        assert_eq!(first.path, b.next_entry(0).unwrap().path);
        a.opened(0, 10.0, 100, (0, None), now, first.path.clone())
            .unwrap();
        assert_ne!(a.next_entry(1).unwrap().path, first.path);
        for _ in 0..20 {
            let bounds = a.clip_bounds(2.0, 10.0, 100);
            assert_eq!(bounds, b.clip_bounds(2.0, 10.0, 100));
            assert!(bounds.0 >= 0 && bounds.1.unwrap() <= 100);
        }
        assert_eq!(a.clip_bounds(0.0, 10.0, 100), (0, None));
        assert!(a.next_entry(4).is_none());
    }

    #[test]
    fn effects_reject_stale_and_failed_results_without_mutating_display() {
        let mut core = PlaybackCore::default();
        let now = Instant::now();
        let open = |core: &mut PlaybackCore, name: &str| match core
            .apply(Command::Replace {
                slot: 0,
                path: name.into(),
                now,
            })
            .unwrap()
        {
            Effect::Open { id, .. } => id,
            _ => unreachable!(),
        };
        let stale = open(&mut core, "old");
        let current = open(&mut core, "new");
        assert!(
            !core
                .open_result(0, stale, Some((10.0, 20, (0, None), "old".into())), now)
                .unwrap()
        );
        assert!(
            core.open_result(0, current, Some((10.0, 20, (0, None), "new".into())), now)
                .unwrap()
        );
        let before = core.slot(0);
        let failed = open(&mut core, "bad");
        assert!(!core.open_result(0, failed, None, now).unwrap());
        assert_eq!(core.slot(0), before);
        assert_eq!(core.identity(0), Some(Path::new("new")));
        core.apply(Command::TogglePause).unwrap();
        let id = match core
            .apply(Command::Seek {
                slot: 0,
                seconds: 1.0,
                now,
            })
            .unwrap()
        {
            Effect::Seek { id, target: 10, .. } => id,
            _ => panic!("expected seek"),
        };
        assert!(!core.seek_result(0, id, false, now));
        assert_eq!(core.slot(0), before);
        assert!(
            core.apply(Command::Seek {
                slot: 4,
                seconds: 1.0,
                now
            })
            .is_err()
        );
        assert!(
            core.apply(Command::Seek {
                slot: 0,
                seconds: f64::NAN,
                now
            })
            .is_err()
        );
        assert_eq!(
            core.apply(Command::Tick {
                slot: 0,
                now: now + Duration::from_secs(1)
            })
            .unwrap(),
            Effect::None
        );
        core.apply(Command::TogglePause).unwrap();
        let id = match core
            .apply(Command::Tick {
                slot: 0,
                now: now + Duration::from_secs(1),
            })
            .unwrap()
        {
            Effect::Read { id, .. } => id,
            _ => panic!("expected read"),
        };
        assert!(!core.read_result(0, id + 1, true, now));
        assert!(core.read_result(0, id, true, now));
        assert_eq!(core.slot(0).unwrap().position, 1);
        assert_eq!(
            core.apply(Command::EndOfMedia { slot: 0, now }).unwrap(),
            Effect::Replace { slot: 0 }
        );
        let fade = core.fade();
        core.apply(Command::ToggleTransition).unwrap();
        assert_ne!(core.fade(), fade);
    }

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
