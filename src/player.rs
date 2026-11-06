use crate::capture::Capture;
use crate::cli::{Args, TransitionMode};
use crate::config::Keybindings;
use crate::controls::{self, Action};
#[cfg(feature = "downloading")]
use crate::downloader::{self, DownloadJob, DownloadedFile};
use crate::playback::{Command, FrameAction, PlaybackCore};
#[cfg(feature = "scene-editing")]
use crate::scenes::{Scene, load_scenes, save_scenes};
use crate::ui::{self, MosaicOptions, RenderOptions, SlotRenderInfo};
use crate::video::{VideoEntry, VideoPool};
use anyhow::Result;
use opencv::core::Mat;
use rand::Rng;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Maintains the playback state, timing, and current frame for a single mosaic tile.
struct SlotState {
    slot_index: usize,
    capture: Option<Capture>,
    frame: Option<Mat>,
    transition_from: Option<Mat>,
    #[cfg(feature = "downloading")]
    download_job: Option<(u64, DownloadJob)>,
    #[cfg(feature = "downloading")]
    downloaded_file: Option<DownloadedFile>,
    error: Option<String>,
    scene_start_mark: Option<i32>,
}

impl SlotState {
    fn new(slot_index: usize) -> Self {
        Self {
            slot_index,
            capture: None,
            frame: None,
            transition_from: None,
            #[cfg(feature = "downloading")]
            download_job: None,
            #[cfg(feature = "downloading")]
            downloaded_file: None,
            error: None,
            scene_start_mark: None,
        }
    }

    fn is_downloading(&self) -> bool {
        #[cfg(feature = "downloading")]
        {
            self.download_job.is_some()
        }
        #[cfg(not(feature = "downloading"))]
        {
            false
        }
    }

    fn to_render_info<'a>(
        &'a self,
        frame_index: i32,
        transition_started_at: Option<Instant>,
        identity: Option<&'a Path>,
    ) -> SlotRenderInfo<'a> {
        SlotRenderInfo {
            slot_index: self.slot_index,
            label: if let Some(error) = &self.error {
                error
            } else if self.is_downloading() {
                "DOWNLOADING..."
            } else if let Some(path) = identity {
                path.file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("[unknown]")
            } else {
                "[empty]"
            },
            frame_index,
            frame: self.frame.as_ref(),
            transition_from: self.transition_from.as_ref(),
            transition_started_at,
        }
    }
}

enum InputState {
    UrlInput { text: String },
    TagInput { text: String, was_paused: bool },
}

/// The main orchestrator managing the OpenCV window, the four video slots,
/// and the playback loop.
pub struct QuadPlayer {
    args: Args,
    pool: VideoPool,
    rng: rand::rngs::StdRng,
    width: i32,
    height: i32,
    slots: Vec<SlotState>,
    core: PlaybackCore,
    show_help: bool,
    fullscreen: bool,
    config: Keybindings,
    input_state: Option<InputState>,
    window: Option<ui::Window>,
}

impl QuadPlayer {
    pub fn new(
        args: Args,
        entries: Vec<VideoEntry>,
        width: i32,
        height: i32,
        config: Keybindings,
    ) -> Result<Self> {
        let fullscreen = args.fullscreen;
        let pool = VideoPool::new(entries, !args.no_shuffle, args.seed);
        let rng = if let Some(s) = args.seed {
            rand::SeedableRng::seed_from_u64(s)
        } else {
            rand::SeedableRng::from_entropy()
        };

        let mut slots = Vec::with_capacity(4);
        for i in 0..4 {
            slots.push(SlotState::new(i));
        }

        Ok(Self {
            args,
            pool,
            rng,
            width,
            height,
            slots,
            core: PlaybackCore::default(),
            show_help: true,
            fullscreen,
            config,
            input_state: None,
            window: None,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        self.window = Some(ui::Window::open(
            "quadplay",
            self.width,
            self.height,
            self.fullscreen,
        )?);

        for i in 0..4 {
            if let Err(error) = self.fill_slot(i) {
                eprintln!("Slot {}: {error:#}", i + 1);
                self.slots[i].error = Some(format!("ERROR: {error}"));
            }
        }

        loop {
            let start = Instant::now();
            for i in 0..4 {
                if let Err(error) = self.advance_slot(i) {
                    eprintln!("Slot {}: {error:#}", i + 1);
                    self.slots[i].error = Some(format!("ERROR: {error}"));
                }
            }

            let mosaic = self.compose_frame()?;
            self.window.as_ref().unwrap().show(&mosaic)?;

            let elapsed = start.elapsed();
            if elapsed > Duration::from_millis(20) {
                println!("WARN: Loop tick took {:?}", elapsed);
            }

            let key = self.window.as_ref().unwrap().key(20)?;
            if key != -1 {
                let key_char = (key & 0xFF) as u8 as char;
                match self.handle_key(key_char) {
                    Ok(false) => break,
                    Ok(true) => {}
                    Err(error) => {
                        eprintln!("Operation failed: {error:#}");
                        self.slots[self.core.selected()].error = Some(format!("ERROR: {error}"));
                    }
                }
            }
        }

        self.close()?;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        for i in 0..self.slots.len() {
            let _ = self.release_slot(i);
        }
        self.window = None;
        Ok(())
    }

    fn handle_key(&mut self, key: char) -> Result<bool> {
        if let Some(mut state) = self.input_state.take() {
            if key == '\r' || key == '\n' {
                match state {
                    InputState::UrlInput { text } => {
                        self.load_url_in_slot(self.core.selected(), text)?;
                    }
                    InputState::TagInput { text, was_paused } => {
                        self.core
                            .apply(Command::SetSelectedPause(was_paused))
                            .expect("valid command");
                        self.finish_scene(self.core.selected(), text)?;
                    }
                }
                return Ok(true);
            } else if key == '\u{1b}' {
                if let InputState::TagInput { was_paused, .. } = state {
                    self.core
                        .apply(Command::SetSelectedPause(was_paused))
                        .expect("valid command");
                }
                return Ok(true);
            } else if key == '\u{8}' || key == '\x7f' {
                match &mut state {
                    InputState::UrlInput { text } => {
                        text.pop();
                    }
                    InputState::TagInput { text, .. } => {
                        text.pop();
                    }
                }
                self.input_state = Some(state);
                return Ok(true);
            } else if key.is_ascii_graphic() || key == ' ' {
                match &mut state {
                    InputState::UrlInput { text } => text.push(key),
                    InputState::TagInput { text, .. } => text.push(key),
                }
                self.input_state = Some(state);
                return Ok(true);
            }
            self.input_state = Some(state);
            return Ok(true);
        }

        match controls::translate(key, &self.config) {
            Some(Action::Select(index)) => {
                self.core.apply(Command::Select(index)).expect("valid slot")
            }
            Some(Action::ToggleSlotPause) => self
                .core
                .apply(Command::ToggleSelectedPause)
                .expect("valid command"),
            Some(Action::Seek(seconds)) => {
                self.seek_slot(self.core.selected(), f64::from(seconds))?
            }
            Some(Action::Load) => {
                self.input_state = Some(InputState::UrlInput {
                    text: String::new(),
                })
            }
            Some(Action::SceneStart) => {
                let index = self.core.selected();
                self.slots[index].scene_start_mark =
                    self.core.slot(index).map(|slot| slot.position);
            }
            Some(Action::SceneStop) => {
                let was_paused = self
                    .core
                    .individually_paused(self.core.selected())
                    .unwrap_or(false);
                self.core
                    .apply(Command::SetSelectedPause(true))
                    .expect("valid command");
                self.input_state = Some(InputState::TagInput {
                    text: String::new(),
                    was_paused,
                });
            }
            Some(Action::Quit) => return Ok(false),
            Some(Action::TogglePause) => {
                self.core
                    .apply(Command::TogglePause)
                    .expect("valid command");
                if !self.core.paused() {
                    let now = Instant::now();
                    self.core.resume_at(now);
                }
            }
            Some(Action::Replace(index)) => self.fill_slot(index)?,
            Some(Action::ReplaceAll) => {
                for index in 0..4 {
                    self.fill_slot(index)?;
                }
            }
            Some(Action::Reshuffle) => self.pool.reshuffle(),
            Some(Action::Help) => self.show_help = !self.show_help,
            Some(Action::Fullscreen) => {
                self.fullscreen = !self.fullscreen;
                self.apply_window_mode()?;
            }
            Some(Action::Transition) => self.args.transition = self.args.transition.next(),
            None => {}
        }
        Ok(true)
    }

    fn load_url_in_slot(&mut self, slot_index: usize, url: String) -> Result<()> {
        if url.starts_with("http://") || url.starts_with("https://") {
            #[cfg(feature = "downloading")]
            {
                let id = self.core.new_request(slot_index).expect("valid slot");
                let job = downloader::start_download(url);
                self.slots[slot_index].download_job = Some((id, job));
                self.slots[slot_index].error = None;
            }
            #[cfg(not(feature = "downloading"))]
            anyhow::bail!("URL downloads are disabled in this build");
        } else {
            // Treat as local file
            self.load_file_into_slot(slot_index, PathBuf::from(url))?;
        }
        Ok(())
    }

    fn load_file_into_slot(&mut self, slot_index: usize, path: PathBuf) -> Result<()> {
        self.core.new_request(slot_index);
        #[cfg(feature = "downloading")]
        {
            self.slots[slot_index].download_job = None;
        }
        let entry = VideoEntry::new(path);
        let previous_frame = self.slots[slot_index].frame.clone();
        if self.open_entry_in_slot(slot_index, entry, previous_frame)? {
            Ok(())
        } else {
            anyhow::bail!("Could not open file in slot {}", slot_index)
        }
    }

    #[cfg(feature = "scene-editing")]
    fn finish_scene(&mut self, slot_index: usize, tags_text: String) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        let start_frame = slot.scene_start_mark.unwrap_or(0);
        let Some(timeline) = self.core.slot(slot_index) else {
            return Ok(());
        };
        let end_frame = timeline.position;

        if let Some(path) = self.core.identity(slot_index) {
            let mut data = load_scenes(path)?;
            let tags: Vec<String> = tags_text
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            data.scenes.push(Scene {
                start_frame,
                end_frame,
                tags,
            });
            let total_frames = timeline.frame_count;
            save_scenes(path, &data, total_frames)?;
            slot.scene_start_mark = None;
            println!("Scene saved.");
        }
        Ok(())
    }

    #[cfg(not(feature = "scene-editing"))]
    fn finish_scene(&mut self, _slot_index: usize, _tags_text: String) -> Result<()> {
        anyhow::bail!("Scene editing is disabled in this build")
    }

    fn seek_slot(&mut self, slot_index: usize, seconds: f64) -> Result<()> {
        let Some(target) = self.core.seek_target(slot_index, seconds) else {
            return Ok(());
        };
        let slot = &mut self.slots[slot_index];
        if let Some(capture) = &mut slot.capture {
            let old_position = self.core.slot(slot_index).unwrap().position;
            let frame = capture.seek_displayed_frame(target, old_position)?;
            slot.frame = Some(frame);
            self.core.seeked(slot_index, target, Instant::now());
        }
        Ok(())
    }

    fn apply_window_mode(&self) -> Result<()> {
        if let Some(window) = &self.window {
            window.set_fullscreen(self.fullscreen)?;
        }
        Ok(())
    }

    fn current_paths(&self, ignored_slot_index: Option<usize>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for i in 0..self.slots.len() {
            if Some(i) == ignored_slot_index {
                continue;
            }
            if let Some(path) = self.core.identity(i) {
                paths.push(path.to_path_buf());
            }
        }
        paths
    }

    fn open_entry_in_slot(
        &mut self,
        slot_index: usize,
        entry: VideoEntry,
        previous_frame: Option<Mat>,
    ) -> Result<bool> {
        let Some(mut capture) = Capture::open(&entry.path)? else {
            return Ok(false);
        };
        let fps = capture.fps();
        let frame_count = capture.frame_count();
        let mut clip_start_frame = 0;
        let mut clip_end_frame = None;

        if self.args.clip_seconds > 0.0 {
            let clip_frames = ((self.args.clip_seconds * fps).round() as i32).max(1);
            if frame_count > clip_frames {
                let max_start = (frame_count - clip_frames - 1).max(0);
                clip_start_frame = self.rng.gen_range(0..=max_start);
            }
            clip_end_frame = Some(clip_start_frame.saturating_add(clip_frames));
            capture.seek(clip_start_frame)?;
        }

        // Validate the first frame before replacing a working slot.
        let Some(first_frame) = capture.read()? else {
            return Ok(false);
        };

        self.core
            .opened(
                slot_index,
                fps,
                frame_count,
                (clip_start_frame, clip_end_frame),
                Instant::now(),
                entry.path,
            )
            .map_err(anyhow::Error::msg)?;

        let slot = &mut self.slots[slot_index];
        slot.capture = Some(capture);
        #[cfg(feature = "downloading")]
        {
            slot.downloaded_file = None;
        }
        slot.error = None;
        slot.frame = Some(first_frame);
        slot.transition_from = None;

        if self.slots[slot_index].frame.is_some() {
            if self.args.transition == TransitionMode::Fade
                && self.args.transition_ms > 0
                && previous_frame.is_some()
            {
                let slot = &mut self.slots[slot_index];
                slot.transition_from = previous_frame;
                self.core.start_transition(slot_index, Instant::now());
            }
            return Ok(true);
        }

        Ok(false)
    }

    fn fill_slot(&mut self, slot_index: usize) -> Result<()> {
        self.core.new_request(slot_index);
        #[cfg(feature = "downloading")]
        {
            self.slots[slot_index].download_job = None;
        }
        self.refill_slot(slot_index)
    }

    /// Automatic playback replacement must not cancel a pending download.
    fn refill_slot(&mut self, slot_index: usize) -> Result<()> {
        let previous_frame = self.slots[slot_index].frame.clone();
        let attempts = self.pool.total_entries().max(1);
        let exclude = self.current_paths(Some(slot_index));

        for _ in 0..attempts {
            let Some(entry) = self.pool.next_entry(&exclude) else {
                anyhow::bail!("No video files available");
            };
            if self.open_entry_in_slot(slot_index, entry, previous_frame.clone())? {
                return Ok(());
            }
        }

        anyhow::bail!("Could not open any readable video files.")
    }

    fn release_slot(&mut self, slot_index: usize) -> Result<()> {
        self.core.new_request(slot_index);
        let slot = &mut self.slots[slot_index];
        slot.capture = None;
        slot.frame = None;
        slot.transition_from = None;
        self.core.clear(slot_index);
        #[cfg(feature = "downloading")]
        {
            slot.download_job = None;
            slot.downloaded_file = None;
        }
        slot.error = None;
        slot.scene_start_mark = None;
        Ok(())
    }

    fn advance_slot(&mut self, slot_index: usize) -> Result<()> {
        #[cfg(feature = "downloading")]
        if let Some((id, job)) = &self.slots[slot_index].download_job {
            let id = *id;
            match job.poll() {
                Ok(Some(file)) => {
                    #[cfg(feature = "downloading")]
                    {
                        self.slots[slot_index].download_job = None;
                    }
                    if !self.core.request_is_current(slot_index, id) {
                        return Ok(());
                    }
                    let entry = VideoEntry::new(file.path().to_owned());
                    let previous = self.slots[slot_index].frame.clone();
                    if !self.open_entry_in_slot(slot_index, entry, previous)? {
                        anyhow::bail!("Could not open downloaded file");
                    }
                    self.slots[slot_index].downloaded_file = Some(file);
                }
                Ok(None) => {}
                Err(error) => {
                    #[cfg(feature = "downloading")]
                    {
                        self.slots[slot_index].download_job = None;
                    }
                    return Err(error);
                }
            }
        }

        if self.slots[slot_index].capture.is_none() && self.slots[slot_index].error.is_some() {
            return Ok(());
        }
        if self.core.slot_paused(slot_index).unwrap_or(true) {
            return Ok(());
        }
        self.read_next_frame(slot_index, true)
    }

    fn read_next_frame(&mut self, slot_index: usize, allow_refill: bool) -> Result<()> {
        let now = Instant::now();

        if self.slots[slot_index].capture.is_none() {
            return if allow_refill {
                self.refill_slot(slot_index)
            } else {
                Ok(())
            };
        }
        if allow_refill {
            match self.core.frame_action(slot_index, now) {
                FrameAction::Wait => return Ok(()),
                FrameAction::Replace => return self.refill_slot(slot_index),
                FrameAction::Read => {}
            }
        }

        let start = Instant::now();
        let frame = self.slots[slot_index].capture.as_mut().unwrap().read()?;
        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(10) {
            println!("WARN: Slot {} capture read took {:?}", slot_index, elapsed);
        }

        if let Some(frame) = frame {
            let slot = &mut self.slots[slot_index];
            slot.frame = Some(frame);
            self.core.frame_read(slot_index, now);

            return Ok(());
        }

        if allow_refill {
            self.refill_slot(slot_index)?;
        }
        Ok(())
    }

    fn compose_frame(&self) -> Result<Mat> {
        let infos: Vec<_> = self
            .slots
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let timeline = self.core.slot(i);
                slot.to_render_info(
                    timeline.map_or(0, |s| s.position),
                    timeline.and_then(|s| s.transition_started_at),
                    self.core.identity(i),
                )
            })
            .collect();
        let options: Vec<_> = (0..4)
            .map(|i| RenderOptions {
                show_help: self.show_help,
                paused: self.core.slot_paused(i).unwrap_or(true),
                fullscreen: self.fullscreen,
                transition: self.args.transition,
                transition_ms: self.args.transition_ms,
            })
            .collect();
        let prompt = self.input_state.as_ref().map(|input| match input {
            InputState::UrlInput { text } => ("Path or URL", text.as_str()),
            InputState::TagInput { text, .. } => ("Scene tags", text.as_str()),
        });
        ui::compose_mosaic(
            &infos,
            &options,
            MosaicOptions {
                width: self.width,
                height: self.height,
                prompt,
            },
        )
    }
}

impl Drop for QuadPlayer {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use opencv::{
        core::{self, Scalar, Size},
        prelude::*,
        videoio,
    };

    #[test]
    fn local_media_failed_replacement_preserves_display_and_paused_seek_updates_it() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sample.avi");
        let fourcc = videoio::VideoWriter::fourcc('M', 'J', 'P', 'G')?;
        let mut writer = videoio::VideoWriter::new(
            path.to_str().unwrap(),
            fourcc,
            10.0,
            Size::new(32, 24),
            true,
        )?;
        assert!(writer.is_opened()?);
        for value in [10.0, 60.0, 110.0] {
            let frame = Mat::new_rows_cols_with_default(24, 32, core::CV_8UC3, Scalar::all(value))?;
            writer.write(&frame)?;
        }
        writer.release()?;
        let args = Args::parse_from(["qplay"]);
        let mut player = QuadPlayer::new(args, vec![], 320, 240, Keybindings::default())?;
        player.load_file_into_slot(0, path.clone())?;
        assert_eq!(player.core.slot(0).unwrap().position, 0);
        player.core.apply(Command::TogglePause).unwrap();
        player.seek_slot(0, 0.2)?;
        assert_eq!(player.core.slot(0).unwrap().position, 2);
        let before = player.slots[0]
            .frame
            .as_ref()
            .unwrap()
            .at_2d::<core::Vec3b>(0, 0)?[0];
        assert!(
            player
                .load_file_into_slot(0, dir.path().join("missing.avi"))
                .is_err()
        );
        assert_eq!(player.core.slot(0).unwrap().position, 2);
        assert_eq!(player.core.identity(0), Some(path.as_path()));
        assert_eq!(
            player.slots[0]
                .frame
                .as_ref()
                .unwrap()
                .at_2d::<core::Vec3b>(0, 0)?[0],
            before
        );
        #[cfg(all(feature = "downloading", unix))]
        {
            use std::os::unix::fs::PermissionsExt;
            let program = dir.path().join("fake-downloader");
            std::fs::write(&program, "#!/bin/sh\nexec sleep 30\n")?;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
            player.pool = VideoPool::new(vec![VideoEntry::new(path.clone())], false, None);
            let id = player.core.new_request(0).unwrap();
            player.slots[0].download_job = Some((
                id,
                crate::downloader::start_with("https://example.invalid".into(), program),
            ));
            player.refill_slot(0)?;
            assert!(player.slots[0].download_job.is_some());
            player.fill_slot(0)?;
            assert!(player.slots[0].download_job.is_none());

            let completed = dir.path().join("completed-downloader");
            std::fs::write(
                &completed,
                format!("#!/bin/sh\ncp '{}' \"$4\"\n", path.display()),
            )?;
            std::fs::set_permissions(&completed, std::fs::Permissions::from_mode(0o700))?;
            let stale_id = player.core.new_request(0).unwrap();
            player.slots[0].download_job = Some((
                stale_id,
                crate::downloader::start_with("https://example.invalid".into(), completed),
            ));
            player.core.new_request(0);
            let deadline = Instant::now() + Duration::from_secs(2);
            while player.slots[0].download_job.is_some() && Instant::now() < deadline {
                player.advance_slot(0)?;
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(player.slots[0].download_job.is_none());
            assert_eq!(player.core.identity(0), Some(path.as_path()));
            assert!(player.slots[0].downloaded_file.is_none());
        }

        Ok(())
    }
}
