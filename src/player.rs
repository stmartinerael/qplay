use crate::cli::{Args, TransitionMode};
use crate::config::Keybindings;
use crate::downloader::{self, DownloadJob};
use crate::scenes::{Scene, load_scenes, save_scenes};
use crate::ui::{Dracula, RenderOptions, SlotRenderInfo, render_slot};
use crate::video::{VideoEntry, VideoPool};
use anyhow::Result;
use opencv::{
    core::{self, Mat, Point, Rect},
    highgui, imgproc,
    prelude::*,
    videoio,
};
use rand::Rng;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

/// Maintains the playback state, timing, and current frame for a single mosaic tile.
pub struct SlotState {
    pub slot_index: usize,
    pub entry: Option<VideoEntry>,
    pub capture: Option<videoio::VideoCapture>,
    pub frame: Option<Mat>,
    pub transition_from: Option<Mat>,
    pub transition_started_at: Option<Instant>,
    pub fps: f64,
    pub frame_count: i32,
    pub frame_index: i32,
    pub clip_start_frame: i32,
    pub clip_end_frame: Option<i32>,
    pub next_frame_at: Option<Instant>,
    pub paused: bool,
    pub download_job: Option<DownloadJob>,
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
            paused: false,
            download_job: None,
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
            label: if self.download_job.is_some() {
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
    TagInput { text: String },
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
    pub paused: bool,
    pub show_help: bool,
    pub fullscreen: bool,
    pub config: Keybindings,
    pub active_slot: usize,
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
            paused: false,
            show_help: true,
            fullscreen,
            config,
            active_slot: 0,
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
            if !self.paused {
                for i in 0..4 {
                    self.advance_slot(i)?;
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
                    Err(error) => eprintln!("Operation failed: {error:#}"),
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
                        self.load_url_in_slot(self.active_slot, text)?;
                    }
                    InputState::TagInput { text } => {
                        self.finish_scene(self.active_slot, text)?;
                    }
                }
                return Ok(true);
            } else if key == '\u{1b}' {
                return Ok(true);
            } else if key == '\u{8}' || key == '\x7f' {
                match &mut state {
                    InputState::UrlInput { text } => {
                        text.pop();
                    }
                    InputState::TagInput { text } => {
                        text.pop();
                    }
                }
                self.input_state = Some(state);
                return Ok(true);
            } else if key.is_ascii_graphic() || key == ' ' {
                match &mut state {
                    InputState::UrlInput { text } => text.push(key),
                    InputState::TagInput { text } => text.push(key),
                }
                self.input_state = Some(state);
                return Ok(true);
            }
            self.input_state = Some(state);
            return Ok(true);
        }

        if key == self.config.select_slot_1 {
            self.active_slot = 0;
            return Ok(true);
        }
        if key == self.config.select_slot_2 {
            self.active_slot = 1;
            return Ok(true);
        }
        if key == self.config.select_slot_3 {
            self.active_slot = 2;
            return Ok(true);
        }
        if key == self.config.select_slot_4 {
            self.active_slot = 3;
            return Ok(true);
        }
        if key == self.config.pause_active {
            let slot = &mut self.slots[self.active_slot];
            slot.paused = !slot.paused;
            return Ok(true);
        }
        if key == self.config.seek_forward_active {
            self.seek_slot(self.active_slot, 5.0)?;
            return Ok(true);
        }
        if key == self.config.seek_backward_active {
            self.seek_slot(self.active_slot, -5.0)?;
            return Ok(true);
        }
        if key == self.config.load_video {
            self.input_state = Some(InputState::UrlInput {
                text: String::new(),
            });
            return Ok(true);
        }
        if key == self.config.mark_scene_start {
            let slot = &mut self.slots[self.active_slot];
            slot.scene_start_mark = Some(slot.frame_index);
            return Ok(true);
        }
        if key == self.config.mark_scene_stop {
            let slot = &mut self.slots[self.active_slot];
            slot.paused = true;
            self.input_state = Some(InputState::TagInput {
                text: String::new(),
            });
            return Ok(true);
        }

        match key {
            'q' | '\u{1b}' => return Ok(false),
            ' ' => {
                self.paused = !self.paused;
                if !self.paused {
                    let now = Instant::now();
                    for slot in &mut self.slots {
                        slot.next_frame_at = Some(now);
                    }
                }
            }
            '1'..='4' => {
                let idx = key.to_digit(10).unwrap() as usize - 1;
                self.fill_slot(idx)?;
            }
            'c' => {
                for i in 0..4 {
                    self.fill_slot(i)?;
                }
            }
            'r' => {
                self.pool.reshuffle();
            }
            'h' => {
                self.show_help = !self.show_help;
            }
            'f' => {
                self.fullscreen = !self.fullscreen;
                self.apply_window_mode()?;
            }
            't' => {
                self.args.transition = self.args.transition.next();
            }
            _ => {}
        }
        Ok(true)
    }

    pub fn load_url_in_slot(&mut self, slot_index: usize, url: String) -> Result<()> {
        if url.starts_with("http://") || url.starts_with("https://") {
            let job = downloader::start_download(url);
            self.release_slot(slot_index)?;
            self.slots[slot_index].download_job = Some(job);
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
        slot.paused = false;
        let start_frame = slot.scene_start_mark.unwrap_or(0);
        let end_frame = slot.frame_index;
        slot.scene_start_mark = None;

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
            println!("Scene saved.");
        }
        Ok(())
    }

    pub fn seek_slot(&mut self, slot_index: usize, seconds: f64) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        if let Some(capture) = &mut slot.capture {
            let fps = slot.fps.max(1.0);
            let frames = (seconds * fps) as i32;
            let mut target = slot.frame_index + frames;
            target = target.max(0);
            if slot.frame_count > 0 {
                target = target.min(slot.frame_count - 1);
            }
            capture.set(videoio::CAP_PROP_POS_FRAMES, target as f64)?;
            slot.frame_index = target;
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
        let path_str = match entry.path.to_str() {
            Some(s) => s,
            None => return Ok(false),
        };

        let mut capture = match videoio::VideoCapture::from_file(path_str, videoio::CAP_ANY) {
            Ok(cap) => cap,
            Err(_) => return Ok(false),
        };

        if !videoio::VideoCapture::is_opened(&capture)? {
            return Ok(false);
        }

        let mut fps = capture.get(videoio::CAP_PROP_FPS)?;
        if fps <= 1.0 || fps.is_nan() {
            fps = 30.0;
        }

        let frame_count = capture.get(videoio::CAP_PROP_FRAME_COUNT)? as i32;
        let mut clip_start_frame = 0;
        let mut clip_end_frame = None;

        if self.args.clip_seconds > 0.0 {
            let clip_frames = (self.args.clip_seconds * fps).round() as i32;
            if frame_count > clip_frames {
                let max_start = (frame_count - clip_frames - 1).max(0);
                clip_start_frame = self.rng.gen_range(0..=max_start);
            }
            clip_end_frame = Some(clip_start_frame + clip_frames);
            capture.set(videoio::CAP_PROP_POS_FRAMES, clip_start_frame as f64)?;
        }

        let slot = &mut self.slots[slot_index];
        slot.entry = Some(entry);
        slot.capture = Some(capture);
        slot.fps = fps;
        slot.frame_count = frame_count;
        slot.frame_index = clip_start_frame;
        slot.clip_start_frame = clip_start_frame;
        slot.clip_end_frame = clip_end_frame;
        slot.frame = None;
        slot.transition_from = None;
        slot.transition_started_at = None;
        slot.next_frame_at = Some(Instant::now());

        self.read_next_frame(slot_index, false)?;
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
        self.release_slot(slot_index)?;
        let attempts = self.pool.total_entries().max(1);
        let exclude = self.current_paths(Some(slot_index));

        for _ in 0..attempts {
            let Some(entry) = self.pool.next_entry(&exclude) else {
                anyhow::bail!("No video files available");
            };
            if self.open_entry_in_slot(slot_index, entry, previous_frame.clone())? {
                return Ok(());
            }
            self.release_slot(slot_index)?;
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
        slot.scene_start_mark = None;
        Ok(())
    }

    pub fn advance_slot(&mut self, slot_index: usize) -> Result<()> {
        // Check for download completion if downloading
        if let Some(job) = &self.slots[slot_index].download_job {
            match job.rx.try_recv() {
                Ok(Ok(path)) => {
                    self.slots[slot_index].download_job = None;
                    return self.load_file_into_slot(slot_index, path);
                }
                Ok(Err(e)) => {
                    eprintln!("Download error for slot {}: {}", slot_index, e);
                    self.slots[slot_index].download_job = None;
                    return self.fill_slot(slot_index);
                }
                Err(TryRecvError::Empty) => {
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => {
                    eprintln!("Download thread disconnected for slot {}", slot_index);
                    self.slots[slot_index].download_job = None;
                    return self.fill_slot(slot_index);
                }
            }
        }

        if self.slots[slot_index].paused {
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

        let mut frame = Mat::default();
        let start = Instant::now();
        let success = self.slots[slot_index]
            .capture
            .as_mut()
            .unwrap()
            .read(&mut frame)?;
        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(10) {
            println!("WARN: Slot {} capture read took {:?}", slot_index, elapsed);
        }

        if success {
            let slot = &mut self.slots[slot_index];
            slot.frame = Some(frame);
            slot.frame_index += 1;
            let interval = Duration::from_secs_f64(1.0 / slot.fps.max(1.0));
            slot.next_frame_at = Some(slot.next_frame_at.unwrap_or(now).max(now) + interval);

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
                    paused: self.paused,
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
