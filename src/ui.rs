use crate::cli::TransitionMode;
use anyhow::{Context, Result};
use opencv::{
    core::{self, Mat, Point, Rect, Scalar, Size},
    imgproc,
    prelude::*,
};
use std::time::{Duration, Instant};

/// Custom theme constants for BGR colors used in overlays and UI elements,
/// inspired by the Dracula color palette.
pub struct Dracula;

impl Dracula {
    pub const BG: Scalar = Scalar::new(24.0, 18.0, 16.0, 0.0); // BGR
    pub const PANEL: Scalar = Scalar::new(48.0, 34.0, 30.0, 0.0);
    pub const OVERLAY: Scalar = Scalar::new(34.0, 25.0, 22.0, 0.0);
    pub const DIVIDER: Scalar = Scalar::new(118.0, 88.0, 82.0, 0.0);
    pub const TEXT: Scalar = Scalar::new(244.0, 245.0, 246.0, 0.0);
    pub const MUTED: Scalar = Scalar::new(181.0, 176.0, 172.0, 0.0);
    pub const ACCENT: Scalar = Scalar::new(255.0, 205.0, 120.0, 0.0);
    pub const SUCCESS: Scalar = Scalar::new(155.0, 223.0, 142.0, 0.0);
    pub const WARNING: Scalar = Scalar::new(126.0, 206.0, 255.0, 0.0);
    pub const SHADOW: Scalar = Scalar::new(12.0, 9.0, 8.0, 0.0);
}

/// Rendering parameters and state for a single slot.
pub struct SlotRenderInfo<'a> {
    pub slot_index: usize,
    pub label: &'a str,
    pub frame_index: i32,
    pub frame: Option<&'a Mat>,
    pub transition_from: Option<&'a Mat>,
    pub transition_started_at: Option<Instant>,
}

/// UI/state options controlling how a single slot is rendered.
#[derive(Clone, Copy)]
pub struct RenderOptions {
    pub show_help: bool,
    pub paused: bool,
    pub fullscreen: bool,
    pub transition: TransitionMode,
    pub transition_ms: u64,
}

/// Layout and input prompt for the mosaic renderer.
pub struct MosaicOptions<'a> {
    pub width: i32,
    pub height: i32,
    pub prompt: Option<(&'a str, &'a str)>,
}

pub fn compose_mosaic(
    slots: &[SlotRenderInfo<'_>],
    options: &[RenderOptions],
    layout: MosaicOptions<'_>,
) -> Result<Mat> {
    anyhow::ensure!(
        slots.len() == 4 && options.len() == 4,
        "a mosaic requires four slots"
    );
    let MosaicOptions {
        width,
        height,
        prompt,
    } = layout;
    let tile_width = width / 2;
    let tile_height = height / 2;
    let mut canvas = Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::BG)?;
    for (i, (info, opts)) in slots.iter().zip(options).enumerate() {
        let x = if i % 2 == 0 { 0 } else { tile_width };
        let y = if i < 2 { 0 } else { tile_height };
        let tile = render_slot(info, tile_width, tile_height, *opts)?;
        let mut dest = Mat::roi_mut(&mut canvas, Rect::new(x, y, tile_width, tile_height))?;
        tile.copy_to(&mut dest)?;
    }
    imgproc::line(
        &mut canvas,
        Point::new(tile_width, 0),
        Point::new(tile_width, height),
        Dracula::DIVIDER,
        2,
        imgproc::LINE_8,
        0,
    )?;
    imgproc::line(
        &mut canvas,
        Point::new(0, tile_height),
        Point::new(width, tile_height),
        Dracula::DIVIDER,
        2,
        imgproc::LINE_8,
        0,
    )?;
    if let Some((label, text)) = prompt {
        imgproc::rectangle(
            &mut canvas,
            Rect::new(0, height - 44, width, 44),
            Dracula::OVERLAY,
            -1,
            imgproc::LINE_8,
            0,
        )?;
        put_text(
            &mut canvas,
            &format!("{label}: {text}"),
            Point::new(12, height - 16),
            0.55,
            Dracula::TEXT,
            1,
        )?;
    }
    Ok(canvas)
}

/// Renders the UI overlay and final frame for a single mosaic slot.
pub fn render_slot(
    info: &SlotRenderInfo,
    width: i32,
    height: i32,
    opts: RenderOptions,
) -> Result<Mat> {
    let RenderOptions {
        show_help,
        paused,
        fullscreen,
        transition,
        transition_ms,
    } = opts;

    let mut canvas = Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Dracula::PANEL)?;

    if let Some(frame) = info.frame {
        let display_frame = render_transition_frame(
            frame,
            info.transition_from,
            info.transition_started_at,
            width,
            height,
            transition,
            transition_ms,
        )?;
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

    let slot_chip = format!("SLOT {}", info.slot_index + 1);
    put_text(
        &mut canvas,
        &slot_chip,
        Point::new(14, 25),
        0.45,
        Dracula::ACCENT,
        1,
    )?;

    let status = ellipsize_middle(info.label, 36);
    put_text(
        &mut canvas,
        &status,
        Point::new(14, 47),
        0.62,
        Dracula::TEXT,
        2,
    )?;

    let position = format!("frame {}", info.frame_index.max(0));
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
pub fn ellipsize_middle(text: &str, max_chars: usize) -> String {
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
pub fn fill_frame(frame: &Mat, width: i32, height: i32) -> Result<Mat> {
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

    Ok(filled)
}

/// Blends the previous frame and current frame based on elapsed transition time.
pub fn render_transition_frame(
    current_frame: &Mat,
    transition_from: Option<&Mat>,
    transition_started_at: Option<Instant>,
    width: i32,
    height: i32,
    transition: TransitionMode,
    transition_ms: u64,
) -> Result<Mat> {
    let current = fill_frame(current_frame, width, height)?;

    if transition != TransitionMode::Fade || transition_ms == 0 {
        return Ok(current);
    }

    let Some(previous_frame) = transition_from else {
        return Ok(current);
    };
    let Some(started_at) = transition_started_at else {
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
pub fn put_text(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ellipsize_middle_short() {
        assert_eq!(ellipsize_middle("hello", 10), "hello");
        assert_eq!(ellipsize_middle("hello", 5), "hello");
    }

    #[test]
    fn test_ellipsize_middle_truncation() {
        assert_eq!(ellipsize_middle("abcdefghij", 7), "ab...ij");
        assert_eq!(ellipsize_middle("short", 2), "...");
        assert_eq!(
            ellipsize_middle("a very long file name that needs trimming.mp4", 20),
            "a very l...mming.mp4"
        );
    }
}
