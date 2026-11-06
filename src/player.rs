use crate::capture::Capture;
use crate::cli::{Args, TransitionMode};
use crate::config::Keybindings;
use crate::controls::{self, Action};
use crate::downloader::{self, DownloadJob, DownloadedFile};
use crate::playback::{Command, PlaybackCore};
use crate::scenes::{Scene, load_scenes, save_scenes};
use crate::ui::{Dracula, RenderOptions, SlotRenderInfo, put_text, render_slot};
use crate::video::{VideoEntry, VideoPool};
use anyhow::Result;
use opencv::{
    core::{self, Mat, Point, Rect},
    highgui, imgproc,
    prelude::*,
};
use rand::Rng;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Maintains the playback state, timing, and current frame for a single mosaic tile.
pub struct SlotState {
    pub slot_index: usize,
    pub entry: Option<VideoEntry>,
    pub capture: Option<Capture>,
    pub frame: Option<Mat>,
    pub transition_from: Option<Mat>,
    pub transition_started_at: Option<Instant>,
    pub fps: f64,
    pub frame_count: i32,
    pub frame_index: i32,
    pub clip_start_frame: i32,
    pub clip_end_frame: Option<i32>,
    pub next_frame_at: Option<Instant>,
    pub download_job: Option<DownloadJob>,
    downloaded_file: Option<DownloadedFile>,
    error: Option<String>,
    pub scene_start_mark: Option<i32>,
}

impl SlotState {
    pub fn new(slot_index: usize) -> Self {
        Self {
            slot_index,
            entry: None,
            capture: None,
            frame: None,
            transition_from: None,
            transition_started_at: None,
            fps: 30.0,
            frame_count: 0,
            frame_index: 0,
            clip_start_frame: 0,
            clip_end_frame: None,
            next_frame_at: None,
            download_job: None,
            downloaded_file: None,
            error: None,
            scene_start_mark: None,
        }
    }

    pub fn label(&self) -> String {
        if self.download_job.is_some() {
            return "DOWNLOADING...".to_string();
        }
        self.entry
            .as_ref()
            .map(|e| e.display_name())
            .unwrap_or_else(|| "[empty]".to_string())
    }

    pub fn to_render_info<'a>(&'a self) -> SlotRenderInfo<'a> {
        SlotRenderInfo {
            slot_index: self.slot_index,
            label: if let Some(error) = &self.error {
                error
            } else if self.download_job.is_some() {
                "DOWNLOADING..."
            } else if let Some(e) = &self.entry {
                e.path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("[unknown]")
            } else {
                "[empty]"
            },
            frame_index: self.frame_index,
            frame: self.frame.as_ref(),
            transition_from: self.transition_from.as_ref(),
            transition_started_at: self.transition_started_at,
        }
    }
}

pub enum InputState {
    UrlInput { text: String },
    TagInput { text: String, was_paused: bool },
}

/// The main orchestrator managing the OpenCV window, the four video slots,
/// and the playback loop.
pub struct QuadPlayer {
    pub args: Args,
    pub window_name: String,
    pub pool: VideoPool,
    pub rng: rand::rngs::StdRng,
    pub width: i32,
    pub height: i32,
    pub slots: Vec<SlotState>,
    core: PlaybackCore,
    pub show_help: bool,
    pub fullscreen: bool,
    pub config: Keybindings,
    pub input_state: Option<InputState>,
    pub explicit_mode: bool,
    window_opened: bool,
}

impl QuadPlayer {
    pub fn new(
        args: Args,
        entries: Vec<VideoEntry>,
        width: i32,
        height: i32,
        config: Keybindings,
        explicit_mode: bool,
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
            window_name: "quadplay".to_string(),
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
            explicit_mode,
            window_opened: false,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        highgui::named_window(&self.window_name, highgui::WINDOW_NORMAL)?;
        highgui::resize_window(&self.window_name, self.width, self.height)?;
        self.window_opened = true;
        self.apply_window_mode()?;

        for i in 0..4 {
            self.fill_slot(i)?;
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
            highgui::imshow(&self.window_name, &mosaic)?;

            let elapsed = start.elapsed();
            if elapsed > Duration::from_millis(20) {
                println!("WARN: Loop tick took {:?}", elapsed);
            }

            let key = highgui::wait_key(20)?;
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

    pub fn close(&mut self) -> Result<()> {
        for i in 0..self.slots.len() {
            let _ = self.release_slot(i);
        }
        if self.window_opened {
            let _ = highgui::destroy_window(&self.window_name);
            self.window_opened = false;
        }
        Ok(())
    }

    pub fn handle_key(&mut self, key: char) -> Result<bool> {
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
                let slot = &mut self.slots[self.core.selected()];
                slot.scene_start_mark = Some(slot.frame_index);
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
                    for slot in &mut self.slots {
                        slot.next_frame_at = Some(now);
                    }
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

    pub fn load_url_in_slot(&mut self, slot_index: usize, url: String) -> Result<()> {
        if url.starts_with("http://") || url.starts_with("https://") {
            let job = downloader::start_download(url);
            self.slots[slot_index].download_job = Some(job);
            self.slots[slot_index].error = None;
        } else {
            // Treat as local file
            self.load_file_into_slot(slot_index, PathBuf::from(url))?;
        }
        Ok(())
    }

    pub fn load_file_into_slot(&mut self, slot_index: usize, path: PathBuf) -> Result<()> {
        let entry = VideoEntry::new(path);
        let previous_frame = self.slots[slot_index].frame.clone();
        if self.open_entry_in_slot(slot_index, entry, previous_frame)? {
            Ok(())
        } else {
            anyhow::bail!("Could not open file in slot {}", slot_index)
        }
    }

    pub fn finish_scene(&mut self, slot_index: usize, tags_text: String) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        let start_frame = slot.scene_start_mark.unwrap_or(0);
        let end_frame = slot.frame_index;

        if let Some(entry) = &slot.entry {
            let mut data = load_scenes(&entry.path)?;
            let tags: Vec<String> = tags_text
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            data.scenes.push(Scene {
                start_frame,
                end_frame,
                tags,
            });
            let total_frames = slot.frame_count;
            save_scenes(&entry.path, &data, total_frames)?;
            slot.scene_start_mark = None;
            println!("Scene saved.");
        }
        Ok(())
    }

    pub fn seek_slot(&mut self, slot_index: usize, seconds: f64) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        if let Some(capture) = &mut slot.capture {
            let target =
                crate::playback::seek_target(slot.frame_index, seconds, slot.fps, slot.frame_count);
            capture.seek(target)?;
            let frame = capture
                .read()?
                .ok_or_else(|| anyhow::anyhow!("Could not read frame after seek"))?;
            slot.frame = Some(frame);
            slot.frame_index = target.saturating_add(1);
            slot.next_frame_at = Some(Instant::now());
        }
        Ok(())
    }

    pub fn apply_window_mode(&self) -> Result<()> {
        let mode = if self.fullscreen {
            highgui::WINDOW_FULLSCREEN as f64
        } else {
            highgui::WINDOW_NORMAL as f64
        };

        highgui::set_window_property(&self.window_name, highgui::WND_PROP_FULLSCREEN, mode)?;
        if !self.fullscreen {
            highgui::resize_window(&self.window_name, self.width, self.height)?;
        }
        Ok(())
    }

    pub fn current_paths(&self, ignored_slot_index: Option<usize>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for (i, slot) in self.slots.iter().enumerate() {
            if Some(i) == ignored_slot_index {
                continue;
            }
            if let Some(entry) = &slot.entry {
                paths.push(entry.path.clone());
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

        let slot = &mut self.slots[slot_index];
        slot.entry = Some(entry);
        slot.capture = Some(capture);
        slot.download_job = None;
        slot.downloaded_file = None;
        slot.error = None;
        slot.fps = fps;
        slot.frame_count = frame_count;
        slot.frame_index = clip_start_frame.saturating_add(1);
        slot.clip_start_frame = clip_start_frame;
        slot.clip_end_frame = clip_end_frame;
        slot.frame = Some(first_frame);
        slot.transition_from = None;
        slot.transition_started_at = None;
        slot.next_frame_at = Some(Instant::now());

        if self.slots[slot_index].frame.is_some() {
            if self.args.transition == TransitionMode::Fade
                && self.args.transition_ms > 0
                && previous_frame.is_some()
            {
                let slot = &mut self.slots[slot_index];
                slot.transition_from = previous_frame;
                slot.transition_started_at = Some(Instant::now());
            }
            return Ok(true);
        }

        Ok(false)
    }

    pub fn fill_slot(&mut self, slot_index: usize) -> Result<()> {
        let previous_frame = self.slots[slot_index].frame.clone();
        self.slots[slot_index].download_job = None;
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

    pub fn release_slot(&mut self, slot_index: usize) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        slot.entry = None;
        slot.capture = None;
        slot.frame = None;
        slot.transition_from = None;
        slot.transition_started_at = None;
        slot.frame_index = 0;
        slot.clip_start_frame = 0;
        slot.clip_end_frame = None;
        slot.next_frame_at = None;
        slot.download_job = None;
        slot.downloaded_file = None;
        slot.error = None;
        slot.scene_start_mark = None;
        Ok(())
    }

    pub fn advance_slot(&mut self, slot_index: usize) -> Result<()> {
        if let Some(job) = &self.slots[slot_index].download_job {
            match job.poll() {
                Ok(Some(file)) => {
                    self.slots[slot_index].download_job = None;
                    let path = file.path().to_owned();
                    self.load_file_into_slot(slot_index, path)?;
                    self.slots[slot_index].downloaded_file = Some(file);
                }
                Ok(None) => {}
                Err(error) => {
                    self.slots[slot_index].download_job = None;
                    return Err(error);
                }
            }
        }

        if self.core.slot_paused(slot_index).unwrap_or(true) {
            return Ok(());
        }
        self.read_next_frame(slot_index, true)
    }

    pub fn read_next_frame(&mut self, slot_index: usize, allow_refill: bool) -> Result<()> {
        let now = Instant::now();

        let needs_read = {
            let slot = &self.slots[slot_index];
            if slot.capture.is_none() {
                if allow_refill {
                    return self.fill_slot(slot_index);
                }
                return Ok(());
            }

            if allow_refill {
                if let Some(next_at) = slot.next_frame_at {
                    now >= next_at
                } else {
                    true
                }
            } else {
                true
            }
        };

        if !needs_read {
            return Ok(());
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
            slot.frame_index = slot.frame_index.saturating_add(1);
            slot.next_frame_at = Some(crate::playback::next_frame_time(
                now,
                slot.next_frame_at,
                slot.fps,
            ));

            if let Some(end) = slot.clip_end_frame
                && slot.frame_index >= end
            {
                return self.fill_slot(slot_index);
            }
            return Ok(());
        }

        if allow_refill {
            self.fill_slot(slot_index)?;
        }
        Ok(())
    }

    pub fn compose_frame(&self) -> Result<Mat> {
        let start = Instant::now();
        let tile_width = self.width / 2;
        let tile_height = self.height / 2;
        let mut canvas =
            Mat::new_rows_cols_with_default(self.height, self.width, core::CV_8UC3, Dracula::BG)?;

        for i in 0..4 {
            let y0 = if i < 2 { 0 } else { tile_height };
            let x0 = if i % 2 == 0 { 0 } else { tile_width };

            let render_info = self.slots[i].to_render_info();
            let tile = render_slot(
                &render_info,
                tile_width,
                tile_height,
                RenderOptions {
                    show_help: self.show_help,
                    paused: self.core.slot_paused(i).unwrap_or(true),
                    fullscreen: self.fullscreen,
                    transition: self.args.transition,
                    transition_ms: self.args.transition_ms,
                },
            )?;

            let roi = Rect::new(x0, y0, tile_width, tile_height);
            let mut dest = Mat::roi_mut(&mut canvas, roi)?;
            tile.copy_to(&mut dest)?;
        }

        imgproc::line(
            &mut canvas,
            Point::new(tile_width, 0),
            Point::new(tile_width, self.height),
            Dracula::DIVIDER,
            2,
            imgproc::LINE_8,
            0,
        )?;
        imgproc::line(
            &mut canvas,
            Point::new(0, tile_height),
            Point::new(self.width, tile_height),
            Dracula::DIVIDER,
            2,
            imgproc::LINE_8,
            0,
        )?;

        if let Some(input) = &self.input_state {
            let (prompt, text) = match input {
                InputState::UrlInput { text } => ("Path or URL", text),
                InputState::TagInput { text, .. } => ("Scene tags", text),
            };
            imgproc::rectangle(
                &mut canvas,
                Rect::new(0, self.height - 44, self.width, 44),
                Dracula::OVERLAY,
                -1,
                imgproc::LINE_8,
                0,
            )?;
            put_text(
                &mut canvas,
                &format!("{prompt}: {text}"),
                Point::new(12, self.height - 16),
                0.55,
                Dracula::TEXT,
                1,
            )?;
        }

        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(16) {
            println!("WARN: compose_frame took {:?}", elapsed);
        }

        Ok(canvas)
    }
}

impl Drop for QuadPlayer {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
