//! OpenCV-backed video capture, with handles kept behind operations.
use anyhow::Result;
use opencv::{core::Mat, prelude::*, videoio};
use std::path::Path;

/// Operations needed to perform a transactional seek with a testable backend.
trait SeekBackend {
    type Frame;
    fn seek_frame(&mut self, index: i32) -> Result<()>;
    fn read_frame(&mut self) -> Result<Option<Self::Frame>>;
}

fn seek_and_decode<B: SeekBackend>(
    backend: &mut B,
    target: i32,
    next_position: i32,
) -> Result<B::Frame> {
    let result = backend
        .seek_frame(target)
        .and_then(|()| backend.read_frame())
        .and_then(|frame| frame.ok_or_else(|| anyhow::anyhow!("Could not read frame after seek")));
    match result {
        Ok(frame) => Ok(frame),
        Err(error) => match backend.seek_frame(next_position) {
            Ok(()) => Err(error),
            Err(restore) => {
                Err(error.context(format!("Could not restore capture position: {restore:#}")))
            }
        },
    }
}

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
    pub fn seek_displayed_frame(&mut self, target: i32, old_position: i32) -> Result<Mat> {
        seek_and_decode(self, target, old_position.saturating_add(1))
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

impl SeekBackend for Capture {
    type Frame = Mat;
    fn seek_frame(&mut self, index: i32) -> Result<()> {
        self.seek(index)
    }
    fn read_frame(&mut self) -> Result<Option<Mat>> {
        self.read()
    }
}

/// Owns current and candidate decoders for all slots. A candidate is committed
/// only after its first frame has been decoded and playback metadata validated.
#[derive(Default)]
pub struct CaptureService {
    current: [Option<Capture>; 4],
    candidate: [Option<Capture>; 4],
}
impl CaptureService {
    pub fn prepare(&mut self, slot: usize, path: &Path) -> Result<Option<(f64, i32)>> {
        self.candidate[slot] = None;
        self.candidate[slot] = Capture::open(path)?;
        Ok(self.candidate[slot]
            .as_ref()
            .map(|c| (c.fps(), c.frame_count())))
    }
    pub fn seek_candidate(&mut self, slot: usize, frame: i32) -> Result<()> {
        self.candidate[slot]
            .as_mut()
            .expect("candidate prepared")
            .seek(frame)
    }
    pub fn read_candidate(&mut self, slot: usize) -> Result<Option<Mat>> {
        self.candidate[slot]
            .as_mut()
            .expect("candidate prepared")
            .read()
    }
    pub fn discard_candidate(&mut self, slot: usize) {
        self.candidate[slot] = None;
    }
    pub fn commit(&mut self, slot: usize) {
        self.current[slot] = self.candidate[slot].take();
    }
    pub fn release(&mut self, slot: usize) {
        self.candidate[slot] = None;
        self.current[slot] = None;
    }
    pub fn has_capture(&self, slot: usize) -> bool {
        self.current[slot].is_some()
    }
    pub fn seek(&mut self, slot: usize, target: i32, position: i32) -> Result<Mat> {
        self.current[slot]
            .as_mut()
            .expect("capture exists")
            .seek_displayed_frame(target, position)
    }
    pub fn read(&mut self, slot: usize) -> Result<Option<Mat>> {
        self.current[slot].as_mut().expect("capture exists").read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        position: i32,
        fail_read: bool,
        fail_restore: bool,
    }
    impl SeekBackend for Fake {
        type Frame = i32;
        fn seek_frame(&mut self, index: i32) -> Result<()> {
            if self.fail_restore && index == 6 {
                anyhow::bail!("restore rejected");
            }
            self.position = index;
            Ok(())
        }
        fn read_frame(&mut self) -> Result<Option<i32>> {
            if self.fail_read {
                Ok(None)
            } else {
                Ok(Some(self.position))
            }
        }
    }
    #[test]
    fn seek_failure_restores_decoder_and_reports_restore_failure() {
        let mut backend = Fake {
            position: 6,
            fail_read: true,
            fail_restore: false,
        };
        assert!(seek_and_decode(&mut backend, 12, 6).is_err());
        assert_eq!(backend.position, 6);
        backend.fail_restore = true;
        assert!(
            format!("{:#}", seek_and_decode(&mut backend, 12, 6).unwrap_err())
                .contains("restore rejected")
        );
        backend.fail_read = false;
        assert_eq!(seek_and_decode(&mut backend, 9, 6).unwrap(), 9);
    }

    #[test]
    fn missing_file_is_not_a_capture() {
        assert!(
            Capture::open(Path::new("/nonexistent/qplay-test-video.mp4"))
                .unwrap()
                .is_none()
        );
    }
}
