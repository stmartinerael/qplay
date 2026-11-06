use crate::capture::CaptureService;
use crate::cli::{Args, TransitionMode};
use crate::config::Keybindings;
use crate::controls::{self, Action};
use crate::playback::{Command, Effect, PlaybackCore};
#[cfg(feature = "scene-editing")]
use crate::scenes::{Scene, load_scenes, save_scenes};
use crate::ui::{self, MosaicOptions, RenderOptions};
use crate::video::VideoEntry;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Maintains the playback state, timing, and current frame for a single mosaic tile.
struct SlotState {
    error: Option<String>,
    scene_start_mark: Option<i32>,
}

impl SlotState {
    fn new() -> Self {
        Self {
            error: None,
            scene_start_mark: None,
        }
    }

    fn label<'a>(&'a self, identity: Option<&'a Path>) -> &'a str {
        if let Some(error) = &self.error {
            error
        } else if let Some(path) = identity {
            path.file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("[unknown]")
        } else {
            "[empty]"
        }
    }
}

enum InputState {
    PathInput { text: String },
    TagInput { text: String, was_paused: bool },
}

/// The main orchestrator managing the OpenCV window, the four video slots,
/// and the playback loop.
pub struct QuadPlayer {
    args: Args,
    width: i32,
    height: i32,
    slots: Vec<SlotState>,
    core: PlaybackCore,
    captures: CaptureService,
    frames: ui::Frames,
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
        let mut core = PlaybackCore::new(entries, !args.no_shuffle, args.seed);
        core.set_fade(args.transition == TransitionMode::Fade);

        let mut slots = Vec::with_capacity(4);
        for _ in 0..4 {
            slots.push(SlotState::new());
        }

        Ok(Self {
            args,
            width,
            height,
            slots,
            core,
            captures: CaptureService::default(),
            frames: ui::Frames::default(),
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

            self.show_frame()?;

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
                    InputState::PathInput { text } => {
                        self.load_file_into_slot(self.core.selected(), PathBuf::from(text))?;
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
                    InputState::PathInput { text } => {
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
                    InputState::PathInput { text } => text.push(key),
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
                self.core.apply(Command::Select(index)).expect("valid slot");
            }
            Some(Action::ToggleSlotPause) => {
                self.core
                    .apply(Command::ToggleSelectedPause)
                    .expect("valid command");
            }
            Some(Action::Seek(seconds)) => {
                self.seek_slot(self.core.selected(), f64::from(seconds))?
            }
            Some(Action::Load) => {
                self.input_state = Some(InputState::PathInput {
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
            Some(Action::Replace(index)) => {
                if let Err(error) = self.fill_slot(index) {
                    eprintln!("Slot {}: {error:#}", index + 1);
                    self.slots[index].error = Some(format!("ERROR: {error}"));
                }
            }
            Some(Action::ReplaceAll) => {
                for index in 0..4 {
                    if let Err(error) = self.fill_slot(index) {
                        eprintln!("Slot {}: {error:#}", index + 1);
                        self.slots[index].error = Some(format!("ERROR: {error}"));
                    }
                }
            }
            Some(Action::Reshuffle) => self.core.reshuffle(),
            Some(Action::Help) => self.show_help = !self.show_help,
            Some(Action::Fullscreen) => {
                self.fullscreen = !self.fullscreen;
                self.apply_window_mode()?;
            }
            Some(Action::Transition) => {
                self.core
                    .apply(Command::ToggleTransition)
                    .expect("valid command");
            }
            None => {}
        }
        Ok(true)
    }

    fn load_file_into_slot(&mut self, slot_index: usize, path: PathBuf) -> Result<()> {
        let entry = VideoEntry::new(path);
        if self.open_entry_in_slot(slot_index, entry)? {
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
        let now = Instant::now();
        let effect = self
            .core
            .apply(Command::Seek {
                slot: slot_index,
                seconds,
                now,
            })
            .map_err(anyhow::Error::msg)?;
        if let Effect::Seek {
            id,
            target,
            old_position,
            ..
        } = effect
        {
            match self.captures.seek(slot_index, target, old_position) {
                Ok(frame) => {
                    if self.core.seek_result(slot_index, id, true, Instant::now()) {
                        self.frames.update(slot_index, frame);
                    }
                }
                Err(error) => {
                    self.core.seek_result(slot_index, id, false, Instant::now());
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn apply_window_mode(&self) -> Result<()> {
        if let Some(window) = &self.window {
            window.set_fullscreen(self.fullscreen)?;
        }
        Ok(())
    }

    fn open_entry_in_slot(&mut self, slot_index: usize, entry: VideoEntry) -> Result<bool> {
        let effect = self
            .core
            .apply(Command::Replace {
                slot: slot_index,
                path: entry.path.clone(),
                now: Instant::now(),
            })
            .map_err(anyhow::Error::msg)?;
        let Effect::Open { id, path, .. } = effect else {
            unreachable!()
        };
        let prepared = (|| -> Result<_> {
            let Some((fps, count)) = self.captures.prepare(slot_index, &path)? else {
                return Ok(None);
            };
            let clip = self.core.clip_bounds(self.args.clip_seconds, fps, count);
            if self.args.clip_seconds > 0.0 {
                self.captures.seek_candidate(slot_index, clip.0)?;
            }
            Ok(self
                .captures
                .read_candidate(slot_index)?
                .map(|frame| (fps, count, clip, frame)))
        })();
        let (fps, count, clip, frame) = match prepared {
            Ok(Some(result)) => result,
            Ok(None) => {
                self.captures.discard_candidate(slot_index);
                self.core
                    .open_result(slot_index, id, None, Instant::now())
                    .map_err(anyhow::Error::msg)?;
                return Ok(false);
            }
            Err(error) => {
                self.captures.discard_candidate(slot_index);
                self.core
                    .open_result(slot_index, id, None, Instant::now())
                    .map_err(anyhow::Error::msg)?;
                return Err(error);
            }
        };
        if !self
            .core
            .open_result(
                slot_index,
                id,
                Some((fps, count, clip, path)),
                Instant::now(),
            )
            .map_err(anyhow::Error::msg)?
        {
            return Ok(false);
        }
        self.captures.commit(slot_index);
        let slot = &mut self.slots[slot_index];
        slot.error = None;
        let fade = self.core.fade() && self.args.transition_ms > 0;
        if self.frames.replace(slot_index, frame, fade) {
            self.core.start_transition(slot_index, Instant::now());
        }
        Ok(true)
    }

    fn fill_slot(&mut self, slot_index: usize) -> Result<()> {
        let attempts = self.core.pool_size().max(1);

        for _ in 0..attempts {
            let Some(entry) = self.core.next_entry(slot_index) else {
                anyhow::bail!("No video files available");
            };
            if self.open_entry_in_slot(slot_index, entry)? {
                return Ok(());
            }
        }

        anyhow::bail!("Could not open any readable video files.")
    }

    fn release_slot(&mut self, slot_index: usize) -> Result<()> {
        let slot = &mut self.slots[slot_index];
        self.captures.release(slot_index);
        self.frames.clear(slot_index);
        self.core.clear(slot_index);
        slot.error = None;
        slot.scene_start_mark = None;
        Ok(())
    }

    fn advance_slot(&mut self, slot_index: usize) -> Result<()> {
        if !self.captures.has_capture(slot_index) && self.slots[slot_index].error.is_some() {
            return Ok(());
        }
        if self.core.slot_paused(slot_index).unwrap_or(true) {
            return Ok(());
        }
        self.read_next_frame(slot_index)
    }

    fn read_next_frame(&mut self, slot_index: usize) -> Result<()> {
        let now = Instant::now();
        match self
            .core
            .apply(Command::Tick {
                slot: slot_index,
                now,
            })
            .map_err(anyhow::Error::msg)?
        {
            Effect::None => Ok(()),
            Effect::Replace { .. } => self.fill_slot(slot_index),
            Effect::Read { id, .. } => match self.captures.read(slot_index) {
                Ok(Some(frame)) => {
                    if self.core.read_result(slot_index, id, true, now) {
                        self.frames.update(slot_index, frame);
                    }
                    Ok(())
                }
                Ok(None) => {
                    self.core.read_result(slot_index, id, false, now);
                    if let Effect::Replace { .. } = self
                        .core
                        .apply(Command::EndOfMedia {
                            slot: slot_index,
                            now,
                        })
                        .map_err(anyhow::Error::msg)?
                    {
                        self.fill_slot(slot_index)?;
                    }
                    Ok(())
                }
                Err(error) => {
                    self.core.read_result(slot_index, id, false, now);
                    Err(error)
                }
            },
            _ => unreachable!(),
        }
    }

    fn show_frame(&self) -> Result<()> {
        let infos: Vec<_> = self
            .slots
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let timeline = self.core.slot(i);
                self.frames.info(
                    i,
                    slot.label(self.core.identity(i)),
                    timeline.map_or(0, |s| s.position),
                    timeline.and_then(|s| s.transition_started_at),
                )
            })
            .collect();
        let options: Vec<_> = (0..4)
            .map(|i| RenderOptions {
                show_help: self.show_help,
                paused: self.core.slot_paused(i).unwrap_or(true),
                fullscreen: self.fullscreen,
                transition: if self.core.fade() {
                    TransitionMode::Fade
                } else {
                    TransitionMode::Cut
                },
                transition_ms: self.args.transition_ms,
            })
            .collect();
        let prompt = self.input_state.as_ref().map(|input| match input {
            InputState::PathInput { text } => ("Local path", text.as_str()),
            InputState::TagInput { text, .. } => ("Scene tags", text.as_str()),
        });
        self.window.as_ref().expect("window open").show_mosaic(
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
        core::{self, Mat, Scalar, Size},
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
        player.handle_key('l')?;
        for key in path.to_str().unwrap().chars() {
            player.handle_key(key)?;
        }
        player.handle_key('\n')?;
        assert_eq!(player.core.slot(0).unwrap().position, 0);
        player.core.apply(Command::TogglePause).unwrap();
        player.seek_slot(0, 0.2)?;
        assert_eq!(player.core.slot(0).unwrap().position, 2);
        let before = player
            .frames
            .current(0)
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
            player
                .frames
                .current(0)
                .unwrap()
                .at_2d::<core::Vec3b>(0, 0)?[0],
            before
        );
        // A local file reaches EOF, then automatically refills without losing its slot.
        player.core = PlaybackCore::new(vec![VideoEntry::new(path.clone())], false, Some(7));
        player.load_file_into_slot(0, path.clone())?;
        player.seek_slot(0, 0.2)?;
        player.read_next_frame(0)?;
        assert_eq!(player.core.slot(0).unwrap().position, 0);
        assert_eq!(player.core.identity(0), Some(path.as_path()));
        // An existing but undecodable input also leaves the current frame intact.
        let broken = dir.path().join("broken.avi");
        std::fs::write(&broken, b"not a video")?;
        assert!(player.load_file_into_slot(0, broken).is_err());
        assert_eq!(player.core.identity(0), Some(path.as_path()));
        assert_eq!(player.core.slot(0).unwrap().position, 0);

        Ok(())
    }
}
