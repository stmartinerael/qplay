//! Playback calculations independent of input, windowing, and media backends.
use std::time::{Duration, Instant};

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
