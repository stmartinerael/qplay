use anyhow::{Context, Result};
use clap::Parser;
use opencv::{
    core::{self, Mat, Point, Rect, Scalar, Size},
    highgui, imgproc,
    prelude::*,
    videoio,
};
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "avi", "webm", "mpg", "mpeg"];

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

#[derive(Parser, Debug)]
#[command(author, version, about = "Play a 2x2 mosaic of local videos", long_about = None)]
struct Args {
    #[arg(default_value = ".")]
    directory: PathBuf,

    #[arg(long, default_value_t = 1280)]
    width: i32,

    #[arg(long, default_value_t = 720)]
    height: i32,

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
}

#[derive(Clone, Debug)]
struct VideoEntry {
    path: PathBuf,
}

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

struct SlotState {
    slot_index: usize,
    entry: Option<VideoEntry>,
    capture: Option<videoio::VideoCapture>,
    frame: Option<Mat>,
    fps: f64,
    frame_index: i32,
    clip_start_frame: i32,
    clip_end_frame: Option<i32>,
    next_frame_at: Option<Instant>,
}

impl SlotState {
    fn new(slot_index: usize) -> Self {
        Self {
            slot_index,
            entry: None,
            capture: None,
            frame: None,
            fps: 30.0,
            frame_index: 0,
            clip_start_frame: 0,
            clip_end_frame: None,
            next_frame_at: None,
        }
    }

    fn label(&self) -> String {
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

struct QuadPlayer {
    args: Args,
    window_name: String,
    pool: VideoPool,
    rng: rand::rngs::StdRng,
    slots: Vec<SlotState>,
    paused: bool,
    show_help: bool,
    fullscreen: bool,
    blank_frame: Mat,
}

impl QuadPlayer {
    fn new(args: Args, entries: Vec<VideoEntry>) -> Result<Self> {
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
            Mat::new_rows_cols_with_default(args.height, args.width, core::CV_8UC3, Dracula::BG)?;

        Ok(Self {
            args,
            window_name: "quadplay".to_string(),
            pool,
            rng,
            slots,
            paused: false,
            show_help: true,
            fullscreen,
            blank_frame,
        })
    }

    fn run(&mut self) -> Result<()> {
        highgui::named_window(&self.window_name, highgui::WINDOW_NORMAL)?;
        highgui::resize_window(&self.window_name, self.args.width, self.args.height)?;
        self.apply_window_mode()?;

        for i in 0..4 {
            self.fill_slot(i)?;
        }

        loop {
            if !self.paused {
                for i in 0..4 {
                    self.advance_slot(i)?;
                }
            }

            let mosaic = self.compose_frame()?;
            highgui::imshow(&self.window_name, &mosaic)?;

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
            _ => {}
        }
        Ok(true)
    }

    fn apply_window_mode(&self) -> Result<()> {
        let mode = if self.fullscreen {
            highgui::WINDOW_FULLSCREEN as f64
        } else {
            highgui::WINDOW_NORMAL as f64
        };

        highgui::set_window_property(&self.window_name, highgui::WND_PROP_FULLSCREEN, mode)?;
        if !self.fullscreen {
            highgui::resize_window(&self.window_name, self.args.width, self.args.height)?;
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
            slot.next_frame_at = Some(Instant::now());

            self.read_next_frame(slot_index, false)?;
            if self.slots[slot_index].frame.is_some() {
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
        let success = self.slots[slot_index]
            .capture
            .as_mut()
            .unwrap()
            .read(&mut frame)?;

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
        let tile_width = self.args.width / 2;
        let tile_height = self.args.height / 2;
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
            )?;

            let roi = Rect::new(x0, y0, tile_width, tile_height);
            let mut dest = Mat::roi_mut(&mut canvas, roi)?;
            tile.copy_to(&mut dest)?;
        }

        imgproc::line(
            &mut canvas,
            Point::new(tile_width, 0),
            Point::new(tile_width, self.args.height),
            Dracula::DIVIDER,
            2,
            imgproc::LINE_8,
            0,
        )?;
        imgproc::line(
            &mut canvas,
            Point::new(0, tile_height),
            Point::new(self.args.width, tile_height),
            Dracula::DIVIDER,
            2,
            imgproc::LINE_8,
            0,
        )?;

        Ok(canvas)
    }
}

fn render_slot(
    slot: &SlotState,
    width: i32,
    height: i32,
    show_help: bool,
    paused: bool,
    fullscreen: bool,
) -> Result<Mat> {
    let mut canvas = Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::PANEL)?;

    if let Some(frame) = &slot.frame {
        let fitted = fit_frame(frame, width, height)?;
        fitted.copy_to(&mut canvas)?;
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
            "1-4 swap  space pause  c all  r reshuffle  f {} screen  h hide  q quit",
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

fn fit_frame(frame: &Mat, width: i32, height: i32) -> Result<Mat> {
    let size = frame.size()?;
    if size.width == 0 || size.height == 0 {
        return Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::PANEL)
            .context("Failed to create empty mat");
    }

    let scale = (width as f64 / size.width as f64).min(height as f64 / size.height as f64);
    let resized_width = (size.width as f64 * scale) as i32;
    let resized_height = (size.height as f64 * scale) as i32;

    let mut resized = Mat::default();
    imgproc::resize(
        frame,
        &mut resized,
        Size::new(resized_width, resized_height),
        0.0,
        0.0,
        imgproc::INTER_AREA,
    )?;

    let mut canvas = Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::PANEL)?;
    let x0 = (width - resized_width) / 2;
    let y0 = (height - resized_height) / 2;

    let roi = Rect::new(x0, y0, resized_width, resized_height);
    let mut dest = Mat::roi_mut(&mut canvas, roi)?;
    resized.copy_to(&mut dest)?;

    Ok(canvas)
}

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
    let entries = discover_videos(&args.directory, args.recursive);
    if entries.is_empty() {
        anyhow::bail!("No video files found in {:?}", args.directory);
    }

    let mut player = QuadPlayer::new(args, entries)?;
    player.run()?;
    Ok(())
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
