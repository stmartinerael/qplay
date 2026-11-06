//! Playback calculations independent of input, windowing, and media backends.
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
    fn seek_respects_known_and_unknown_bounds() {
        assert_eq!(seek_target(10, -5.0, 30.0, 100), 0);
        assert_eq!(seek_target(10, 5.0, 30.0, 100), 99);
        assert_eq!(seek_target(10, 5.0, 30.0, 0), 160);
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
