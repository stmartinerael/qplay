//! QuadPlayer: A 2x2 mosaic video player using OpenCV.
//!
//! This application searches for video files in a specified directory
//! and plays four of them simultaneously in a 2x2 grid. It features smooth transitions,
//! random shuffling, and interactive playback controls.

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use opencv::{
    core::{self, Mat, Point, Rect, Scalar, Size},
    highgui, imgproc,
    prelude::*,
    videoio,
};
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use walkdir::WalkDir;

/// Supported video file extensions.
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "avi", "webm", "mpg", "mpeg"];

mod config;
mod downloader;
mod scenes;

use config::Keybindings;
use downloader::DownloadJob;
use scenes::{load_scenes, save_scenes, Scene, SceneData};

/// Custom theme constants for BGR colors used in overlays and UI elements,
/// inspired by the Dracula color palette.
struct Dracula;
impl Dracula {
    const BG: Scalar = Scalar::new(24.0, 18.0, 16.0, 0.0); // BGR
    const PANEL: Scalar = Scalar::new(48.0, 34.0, 30.0, 0.0);
    const OVERLAY: Scalar = Scalar::new(34.0, 25.0, 22.0, 0.0);
    const DIVIDER: Scalar = Scalar::new(118.0, 88.0, 82.0, 0.0);
    const TEXT: Scalar = Scalar::new(244.0, 245.0, 246.0, 0.0);
    const MUTED: Scalar = Scalar::new(181.0, 176.0, 172.0, 0.0);
    const ACCENT: Scalar = Scalar::new(255.0, 205.0, 120.0, 0.0);
    const SUCCESS: Scalar = Scalar::new(155.0, 223.0, 142.0, 0.0);
    const WARNING: Scalar = Scalar::new(126.0, 206.0, 255.0, 0.0);
    const SHADOW: Scalar = Scalar::new(12.0, 9.0, 8.0, 0.0);
}

/// Command-line arguments for the QuadPlayer application.
#[derive(Parser, Debug)]
#[command(author, version, about = "Play a 2x2 mosaic of local videos", long_about = None)]
struct Args {
    #[arg(num_args = 0.., default_value = ".")]
    inputs: Vec<String>,

    #[arg(long)]
    width: Option<i32>,

    #[arg(long)]
    height: Option<i32>,

    #[arg(long, default_value_t = 0.0)]
    clip_seconds: f64,

    #[arg(long)]
    seed: Option<u64>,

    #[arg(long)]
    recursive: bool,

    #[arg(long)]
    no_shuffle: bool,

    #[arg(long, help = "Start in full-screen mode")]
    fullscreen: bool,

    #[arg(long, value_enum, default_value_t = TransitionMode::Fade)]
    transition: TransitionMode,

    #[arg(
        long,
        default_value_t = 300,
        help = "Transition duration in milliseconds"
    )]
    transition_ms: u64,
}

/// Represents a single video file discovered in the target directory.
#[derive(Clone, Debug)]
struct VideoEntry {
    /// The absolute or relative path to the video file.
    path: PathBuf,
}

/// The visual transition style to use when swapping videos.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum TransitionMode {
    /// Instant cut with no interpolation.
    Cut,
    /// Smooth crossfade between the old and new frames.
    Fade,
}

impl TransitionMode {
    fn next(self) -> Self {
        match self {
            Self::Cut => Self::Fade,
            Self::Fade => Self::Cut,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Cut => "CUT",
            Self::Fade => "FADE",
        }
    }
}

/// Searches a directory for supported video files.
///
/// Filters out partial downloads (e.g., `.part` files) and limits by extension.
/// If `recursive` is true, subdirectories are also searched.
fn discover_videos(directory: &Path, recursive: bool) -> Vec<VideoEntry> {
    let mut entries = Vec::new();
    let walker = if recursive {
        WalkDir::new(directory)
    } else {
        WalkDir::new(directory).max_depth(1)
    };

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
                    && !path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .ends_with(".part")
                {
                    entries.push(VideoEntry { path });
                }
            }
        }
    }
    entries.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
    entries
}

/// Manages a collection of discovered videos, handling shuffling and selection
/// to ensure diverse playback without immediate repetition.
struct VideoPool {
    entries: Vec<VideoEntry>,
    shuffle: bool,
    rng: rand::rngs::StdRng,
    queue: Vec<VideoEntry>,
}

impl VideoPool {
    fn new(entries: Vec<VideoEntry>, shuffle: bool, seed: Option<u64>) -> Self {
        let rng = if let Some(s) = seed {
            rand::rngs::StdRng::seed_from_u64(s)
        } else {
            rand::rngs::StdRng::from_entropy()
        };
        let mut pool = Self {
            entries,
            shuffle,
            rng,
            queue: Vec::new(),
        };
        pool.refill_queue();
        pool
    }

    fn refill_queue(&mut self) {
        self.queue = self.entries.clone();
        if self.shuffle {
            self.queue.shuffle(&mut self.rng);
        }
    }

    fn next_entry(&mut self, exclude: &[PathBuf]) -> VideoEntry {
        if self.queue.is_empty() {
            self.refill_queue();
        }

        for i in 0..self.queue.len() {
            if !exclude.contains(&self.queue[i].path) || exclude.len() >= self.entries.len() {
                return self.queue.remove(i);
            }
        }

        self.queue.remove(0)
    }

    fn reshuffle(&mut self) {
        self.refill_queue();
    }
}

/// Maintains the playback state, timing, and current frame for a single mosaic tile.
struct SlotState {
    slot_index: usize,
    entry: Option<VideoEntry>,
    capture: Option<videoio::VideoCapture>,
    frame: Option<Mat>,
    transition_from: Option<Mat>,
    transition_started_at: Option<Instant>,
    fps: f64,
    frame_count: i32,
    frame_index: i32,
    clip_start_frame: i32,
    clip_end_frame: Option<i32>,
    next_frame_at: Option<Instant>,
    paused: bool,
    download_job: Option<DownloadJob>,
    scene_start_mark: Option<i32>,
}

impl SlotState {
    fn new(slot_index: usize) -> Self {
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

    fn label(&self) -> String {
        if self.download_job.is_some() {
            return "DOWNLOADING...".to_string();
        }
        self.entry
            .as_ref()
            .map(|e| {
                e.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "[empty]".to_string())
    }
}

/// The main orchestrator managing the OpenCV window, the four video slots,
/// and the playback loop.
struct QuadPlayer {
    args: Args,
    window_name: String,
    pool: VideoPool,
    rng: rand::rngs::StdRng,
    width: i32,
    height: i32,
    slots: Vec<SlotState>,
    paused: bool,
    show_help: bool,
    fullscreen: bool,
    blank_frame: Mat,
    config: Keybindings,
    active_slot: usize,
    input_state: Option<InputState>,
    explicit_mode: bool,
}

enum InputState {
    UrlInput { text: String },
    TagInput { text: String },
}

impl QuadPlayer {
    fn new(
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
            rand::rngs::StdRng::seed_from_u64(s)
        } else {
            rand::rngs::StdRng::from_entropy()
        };

        let mut slots = Vec::new();
        for i in 0..4 {
            slots.push(SlotState::new(i));
        }

        let blank_frame =
            Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::BG)?;

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
            blank_frame,
            config,
            active_slot: 0,
            input_state: None,
            explicit_mode,
        })
    }

    fn run(&mut self) -> Result<()> {
        highgui::named_window(&self.window_name, highgui::WINDOW_NORMAL)?;
        highgui::resize_window(&self.window_name, self.width, self.height)?;
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
                if !self.handle_key(key_char)? {
                    break;
                }
            }
        }

        self.close()?;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        for i in 0..4 {
            self.release_slot(i)?;
        }
        highgui::destroy_all_windows()?;
        Ok(())
    }

    fn handle_key(&mut self, key: char) -> Result<bool> {
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
                    InputState::UrlInput { text } => { text.pop(); }
                    InputState::TagInput { text } => { text.pop(); }
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

        if key == self.config.select_slot_1 { self.active_slot = 0; return Ok(true); }
        if key == self.config.select_slot_2 { self.active_slot = 1; return Ok(true); }
        if key == self.config.select_slot_3 { self.active_slot = 2; return Ok(true); }
        if key == self.config.select_slot_4 { self.active_slot = 3; return Ok(true); }
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
            self.input_state = Some(InputState::UrlInput { text: String::new() });
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
            self.input_state = Some(InputState::TagInput { text: String::new() });
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

    fn load_url_in_slot(&mut self, slot_index: usize, url: String) -> Result<()> {
        if url.starts_with("http://") || url.starts_with("https://") {
            let job = downloader::start_download(url);
            self.release_slot(slot_index)?;
            self.slots[slot_index].download_job = Some(job);
        } else {
            // Treat as local file
            self.release_slot(slot_index)?;
            self.load_file_into_slot(slot_index, PathBuf::from(url))?;
        }
        Ok(())
    }

    fn finish_scene(&mut self, slot_index: usize, tags_text: String) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        slot.paused = false;
        let start_frame = slot.scene_start_mark.unwrap_or(0);
        let end_frame = slot.frame_index;
        slot.scene_start_mark = None;
        
        if let Some(entry) = &slot.entry {
            let mut data = load_scenes(&entry.path).unwrap_or_default();
            let tags: Vec<String> = tags_text.split_whitespace().map(|s| s.to_string()).collect();
            data.scenes.push(Scene {
                start_frame,
                end_frame,
                tags,
            });
            let total_frames = slot.frame_count;
            if let Err(e) = save_scenes(&entry.path, &data, total_frames) {
                println!("Error saving scene: {}", e);
            } else {
                println!("Scene saved.");
            }
        }
        Ok(())
    }

    fn seek_slot(&mut self, slot_index: usize, seconds: f64) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        if let Some(capture) = &mut slot.capture {
            let fps = slot.fps.max(1.0);
            let frames = (seconds * fps) as i32;
            let mut target = slot.frame_index + frames;
            target = target.clamp(0, slot.frame_count - 1);
            capture.set(videoio::CAP_PROP_POS_FRAMES, target as f64)?;
            slot.frame_index = target;
        }
        Ok(())
    }

    fn apply_window_mode(&self) -> Result<()> {
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

    fn current_paths(&self, ignored_slot_index: Option<usize>) -> Vec<PathBuf> {
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

    fn fill_slot(&mut self, slot_index: usize) -> Result<()> {
        let previous_frame = self.slots[slot_index].frame.clone();
        self.release_slot(slot_index)?;
        let attempts = self.pool.entries.len().max(1);
        let exclude = self.current_paths(Some(slot_index));

        for _ in 0..attempts {
            let entry = self.pool.next_entry(&exclude);
            let mut capture = videoio::VideoCapture::from_file(
                entry.path.to_str().context("Invalid path")?,
                videoio::CAP_ANY,
            )?;

            if !videoio::VideoCapture::is_opened(&capture)? {
                continue;
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
                    slot.transition_from = previous_frame.clone();
                    slot.transition_started_at = Some(Instant::now());
                }
                return Ok(());
            }
            self.release_slot(slot_index)?;
        }

        anyhow::bail!("Could not open any readable video files.")
    }

    fn release_slot(&mut self, slot_index: usize) -> Result<()> {
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
        Ok(())
    }

    fn advance_slot(&mut self, slot_index: usize) -> Result<()> {
        self.read_next_frame(slot_index, true)
    }

    fn read_next_frame(&mut self, slot_index: usize, allow_refill: bool) -> Result<()> {
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

            if let Some(end) = slot.clip_end_frame {
                if slot.frame_index >= end {
                    return self.fill_slot(slot_index);
                }
            }
            return Ok(());
        }

        if allow_refill {
            self.fill_slot(slot_index)?;
        }
        Ok(())
    }

    fn compose_frame(&self) -> Result<Mat> {
        let start = Instant::now();
        let tile_width = self.width / 2;
        let tile_height = self.height / 2;
        let mut canvas = self.blank_frame.clone();

        for i in 0..4 {
            let y0 = if i < 2 { 0 } else { tile_height };
            let x0 = if i % 2 == 0 { 0 } else { tile_width };

            let tile = render_slot(
                &self.slots[i],
                tile_width,
                tile_height,
                self.show_help,
                self.paused,
                self.fullscreen,
                self.args.transition,
                self.args.transition_ms,
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

/// Renders the UI overlay and final frame for a single mosaic slot.
///
/// This includes the video frame itself (with potential transition effects),
/// bounding boxes, and informational text elements (labels, status, paused state).
fn render_slot(
    slot: &SlotState,
    width: i32,
    height: i32,
    show_help: bool,
    paused: bool,
    fullscreen: bool,
    transition: TransitionMode,
    transition_ms: u64,
) -> Result<Mat> {
    let mut canvas = Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::PANEL)?;

    if let Some(frame) = &slot.frame {
        let display_frame =
            render_transition_frame(slot, frame, width, height, transition, transition_ms)?;
        display_frame.copy_to(&mut canvas)?;
    }

    imgproc::rectangle(
        &mut canvas,
        Rect::new(0, 0, width, height),
        Dracula::DIVIDER,
        1,
        imgproc::LINE_8,
        0,
    )?;

    let top_bar_height = 66;
    let footer_height = if paused || show_help { 40 } else { 0 };
    if top_bar_height > 0 {
        imgproc::rectangle(
            &mut canvas,
            Rect::new(0, 0, width, top_bar_height),
            Dracula::OVERLAY,
            -1,
            imgproc::LINE_8,
            0,
        )?;
    }
    if footer_height > 0 {
        imgproc::rectangle(
            &mut canvas,
            Rect::new(0, height - footer_height, width, footer_height),
            Dracula::OVERLAY,
            -1,
            imgproc::LINE_8,
            0,
        )?;
    }

    let slot_chip = format!("SLOT {}", slot.slot_index + 1);
    put_text(
        &mut canvas,
        &slot_chip,
        Point::new(14, 25),
        0.45,
        Dracula::ACCENT,
        1,
    )?;

    let status = ellipsize_middle(&slot.label(), 36);
    put_text(
        &mut canvas,
        &status,
        Point::new(14, 47),
        0.62,
        Dracula::TEXT,
        2,
    )?;

    let position = format!("frame {}", slot.frame_index.max(0));
    put_text(
        &mut canvas,
        &position,
        Point::new(14, 64),
        0.42,
        Dracula::MUTED,
        1,
    )?;

    let transition_text = format!("transition {}", transition.label());
    put_text(
        &mut canvas,
        &transition_text,
        Point::new(width - 170, 64),
        0.42,
        Dracula::MUTED,
        1,
    )?;

    let state_chip = if paused { "PAUSED" } else { "LIVE" };
    let state_color = if paused {
        Dracula::WARNING
    } else {
        Dracula::SUCCESS
    };
    let chip_width = if paused { 92 } else { 68 };
    let chip_rect = Rect::new(width - chip_width - 14, 12, chip_width, 28);
    imgproc::rectangle(
        &mut canvas,
        chip_rect,
        Dracula::PANEL,
        -1,
        imgproc::LINE_8,
        0,
    )?;
    imgproc::rectangle(&mut canvas, chip_rect, state_color, 1, imgproc::LINE_8, 0)?;
    put_text(
        &mut canvas,
        state_chip,
        Point::new(chip_rect.x + 12, chip_rect.y + 19),
        0.42,
        state_color,
        1,
    )?;

    if paused {
        put_text(
            &mut canvas,
            "Playback paused. Press space to resume.",
            Point::new(14, height - 15),
            0.46,
            Dracula::WARNING,
            1,
        )?;
    } else if show_help {
        let fullscreen_hint = if fullscreen { "window" } else { "full" };
        let help_text = format!(
            "1-4 swap  space pause  c all  r reshuffle  t transition  f {} screen  h hide  q quit",
            fullscreen_hint
        );
        put_text(
            &mut canvas,
            &help_text,
            Point::new(14, height - 15),
            0.45,
            Dracula::TEXT,
            1,
        )?;
    }

    Ok(canvas)
}

/// Truncates a string by replacing its middle section with `...` if it exceeds `max_chars`.
fn ellipsize_middle(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }

    if max_chars <= 3 {
        return "...".to_string();
    }

    let left = (max_chars - 3) / 2;
    let right = max_chars - 3 - left;
    let start: String = chars.iter().take(left).collect();
    let end: String = chars[chars.len() - right..].iter().collect();
    format!("{}...{}", start, end)
}

/// Resizes and crops a frame to perfectly fill the specified dimensions
/// while maintaining the original aspect ratio.
fn fill_frame(frame: &Mat, width: i32, height: i32) -> Result<Mat> {
    let start = Instant::now();
    let size = frame.size()?;
    if size.width == 0 || size.height == 0 {
        return Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::PANEL)
            .context("Failed to create empty mat");
    }

    let scale = (width as f64 / size.width as f64).max(height as f64 / size.height as f64);
    let resized_width = ((size.width as f64 * scale).round() as i32).max(width);
    let resized_height = ((size.height as f64 * scale).round() as i32).max(height);

    let mut resized = Mat::default();
    imgproc::resize(
        frame,
        &mut resized,
        Size::new(resized_width, resized_height),
        0.0,
        0.0,
        imgproc::INTER_LINEAR,
    )?;

    let x0 = ((resized_width - width) / 2).max(0);
    let y0 = ((resized_height - height) / 2).max(0);
    let roi = Rect::new(x0, y0, width, height);
    let cropped = Mat::roi(&resized, roi)?;
    let mut filled = Mat::default();
    cropped.copy_to(&mut filled)?;

    let elapsed = start.elapsed();
    if elapsed > Duration::from_millis(10) {
        println!("WARN: fill_frame (resize) took {:?}", elapsed);
    }

    Ok(filled)
}

/// Blends the previous frame and current frame based on elapsed transition time.
///
/// Returns the pure current frame if no transition is active or the transition has completed.
fn render_transition_frame(
    slot: &SlotState,
    current_frame: &Mat,
    width: i32,
    height: i32,
    transition: TransitionMode,
    transition_ms: u64,
) -> Result<Mat> {
    let current = fill_frame(current_frame, width, height)?;

    if transition != TransitionMode::Fade || transition_ms == 0 {
        return Ok(current);
    }

    let Some(previous_frame) = &slot.transition_from else {
        return Ok(current);
    };
    let Some(started_at) = slot.transition_started_at else {
        return Ok(current);
    };

    let elapsed = Instant::now().saturating_duration_since(started_at);
    if elapsed >= Duration::from_millis(transition_ms) {
        return Ok(current);
    }

    let previous = fill_frame(previous_frame, width, height)?;
    let progress = elapsed.as_secs_f64() / Duration::from_millis(transition_ms).as_secs_f64();
    let alpha = progress.clamp(0.0, 1.0);
    let beta = 1.0 - alpha;
    let mut blended = Mat::default();
    core::add_weighted(&current, alpha, &previous, beta, 0.0, &mut blended, -1)?;
    Ok(blended)
}

/// Draws text on a `Mat` with a drop shadow effect for better readability.
fn put_text(
    image: &mut Mat,
    text: &str,
    origin: Point,
    scale: f64,
    color: Scalar,
    thickness: i32,
) -> Result<()> {
    imgproc::put_text(
        image,
        text,
        origin,
        imgproc::FONT_HERSHEY_SIMPLEX,
        scale,
        Scalar::new(0.0, 0.0, 0.0, 0.0),
        thickness + 2,
        imgproc::LINE_AA,
        false,
    )?;
    imgproc::put_text(
        image,
        text,
        origin,
        imgproc::FONT_HERSHEY_SIMPLEX,
        scale,
        Dracula::SHADOW,
        thickness + 3,
        imgproc::LINE_AA,
        false,
    )?;
    imgproc::put_text(
        image,
        text,
        origin,
        imgproc::FONT_HERSHEY_SIMPLEX,
        scale,
        color,
        thickness,
        imgproc::LINE_AA,
        false,
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = config::load_keybindings().unwrap_or_default();

    let mut explicit_mode = false;
    let entries = if args.inputs.is_empty()
        || (args.inputs.len() == 1 && Path::new(&args.inputs[0]).is_dir())
    {
        let dir = if args.inputs.is_empty() {
            Path::new(".")
        } else {
            Path::new(&args.inputs[0])
        };
        discover_videos(dir, args.recursive)
    } else {
        explicit_mode = true;
        args.inputs
            .iter()
            .map(|s| VideoEntry {
                path: PathBuf::from(s),
            })
            .collect()
    };

    if entries.is_empty() && !explicit_mode {
        anyhow::bail!("No video files found");
    }

    let (width, height) = resolve_dimensions(&args)?;
    let mut player = QuadPlayer::new(args, entries, width, height, config, explicit_mode)?;
    player.run()?;
    Ok(())
}

/// Determines the target window dimensions based on CLI arguments and display detection.
fn resolve_dimensions(args: &Args) -> Result<(i32, i32)> {
    let default_width = 1280;
    let default_height = 720;

    if args.fullscreen {
        let display = detect_display_dimensions().unwrap_or((default_width, default_height));
        let width = args.width.unwrap_or(display.0);
        let height = args.height.unwrap_or(display.1);
        return Ok((width, height));
    }

    Ok((
        args.width.unwrap_or(default_width),
        args.height.unwrap_or(default_height),
    ))
}

/// Queries the X11 server via `xdpyinfo` to detect the current display resolution.
fn detect_display_dimensions() -> Result<(i32, i32)> {
    let output = Command::new("xdpyinfo")
        .output()
        .context("Failed to query display dimensions with xdpyinfo")?;

    if !output.status.success() {
        anyhow::bail!("xdpyinfo exited with status {}", output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("dimensions:") {
            let dimensions = rest.trim();
            if let Some((width, height)) = dimensions.split_once('x') {
                let width = width
                    .trim()
                    .parse::<i32>()
                    .context("Failed to parse display width")?;
                let height = height
                    .split_whitespace()
                    .next()
                    .context("Missing display height")?
                    .parse::<i32>()
                    .context("Failed to parse display height")?;
                return Ok((width, height));
            }
        }
    }

    anyhow::bail!("Could not find display dimensions in xdpyinfo output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn test_discover_videos() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path();

        // Create dummy files
        File::create(path.join("video1.mp4"))?;
        File::create(path.join("video2.MOV"))?; // Case insensitive
        File::create(path.join("image.jpg"))?; // Wrong extension
        File::create(path.join("video3.part"))?; // Excluded suffix

        let subdir = path.join("subdir");
        std::fs::create_dir(&subdir)?;
        File::create(subdir.join("subvideo.mkv"))?;

        // Test non-recursive
        let entries = discover_videos(path, false);
        assert_eq!(entries.len(), 2);
        let names: Vec<_> = entries
            .iter()
            .map(|e| e.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"video1.mp4"));
        assert!(names.contains(&"video2.MOV"));

        // Test recursive
        let entries_rec = discover_videos(path, true);
        assert_eq!(entries_rec.len(), 3);

        Ok(())
    }

    #[test]
    fn test_video_pool_logic() {
        let entries = vec![
            VideoEntry {
                path: PathBuf::from("a.mp4"),
            },
            VideoEntry {
                path: PathBuf::from("b.mp4"),
            },
            VideoEntry {
                path: PathBuf::from("c.mp4"),
            },
        ];

        let mut pool = VideoPool::new(entries.clone(), false, Some(42));

        // Initial queue should match entries when not shuffled
        assert_eq!(pool.queue.len(), 3);
        assert_eq!(pool.queue[0].path, PathBuf::from("a.mp4"));

        // Test next_entry with exclusion
        let exclude = vec![PathBuf::from("a.mp4")];
        let next = pool.next_entry(&exclude);
        assert_eq!(next.path, PathBuf::from("b.mp4"));
        assert_eq!(pool.queue.len(), 2);

        // Test exhaustion and refill
        pool.next_entry(&[]); // gets 'c.mp4' (since 'b' was already removed)
        assert_eq!(pool.queue.len(), 1); // wait, b was removed, queue was [a, c]
                                         // Actually:
                                         // Start: [a, b, c]
                                         // next_entry(exclude: [a]) -> returns b, queue: [a, c]
                                         // next_entry(exclude: []) -> returns a, queue: [c]
                                         // next_entry(exclude: []) -> returns c, queue: []
                                         // next_entry(exclude: []) -> refills, queue: [a, b, c], returns a, queue: [b, c]

        let mut pool2 = VideoPool::new(entries, false, None);
        let _ = pool2.next_entry(&[]); // a
        let _ = pool2.next_entry(&[]); // b
        let _ = pool2.next_entry(&[]); // c
        assert!(pool2.queue.is_empty());

        let next_refill = pool2.next_entry(&[]);
        assert_eq!(next_refill.path, PathBuf::from("a.mp4"));
        assert_eq!(pool2.queue.len(), 2);
    }

    #[test]
    fn test_video_pool_shuffle() {
        let entries = (0..10)
            .map(|i| VideoEntry {
                path: PathBuf::from(format!("{}.mp4", i)),
            })
            .collect::<Vec<_>>();

        let pool_shuffled = VideoPool::new(entries.clone(), true, Some(123));
        let pool_sorted = VideoPool::new(entries.clone(), false, Some(123));

        assert_ne!(
            pool_shuffled
                .queue
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            pool_sorted
                .queue
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>()
        );
    }
}
