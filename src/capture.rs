//! OpenCV-backed video capture, with handles kept behind operations.
use anyhow::Result;
use opencv::{core::Mat, prelude::*, videoio};
use std::path::Path;

pub struct Capture {
    handle: videoio::VideoCapture,
    fps: f64,
    frames: i32,
}
impl Capture {
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let Some(path) = path.to_str() else {
            return Ok(None);
        };
        let handle = match videoio::VideoCapture::from_file(path, videoio::CAP_ANY) {
            Ok(handle) => handle,
            Err(_) => return Ok(None),
        };
        if !handle.is_opened()? {
            return Ok(None);
        }
        let raw_fps = handle.get(videoio::CAP_PROP_FPS)?;
        let fps = if raw_fps.is_finite() && raw_fps > 1.0 {
            raw_fps
        } else {
            30.0
        };
        let raw_frames = handle.get(videoio::CAP_PROP_FRAME_COUNT)?;
        let frames = if raw_frames.is_finite() && raw_frames > 0.0 {
            raw_frames as i32
        } else {
            0
        };
        Ok(Some(Self {
            handle,
            fps,
            frames,
        }))
    }
    pub fn fps(&self) -> f64 {
        self.fps
    }
    pub fn frame_count(&self) -> i32 {
        self.frames
    }
    pub fn seek(&mut self, index: i32) -> Result<()> {
        if !self
            .handle
            .set(videoio::CAP_PROP_POS_FRAMES, f64::from(index))?
        {
            anyhow::bail!("video backend rejected seek");
        }
        Ok(())
    }
    pub fn read(&mut self) -> Result<Option<Mat>> {
        let mut frame = Mat::default();
        if self.handle.read(&mut frame)? && !frame.empty() {
            Ok(Some(frame))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_file_is_not_a_capture() {
        assert!(
            Capture::open(Path::new("/nonexistent/qplay-test-video.mp4"))
                .unwrap()
                .is_none()
        );
    }
}
