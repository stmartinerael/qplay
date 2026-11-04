use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use std::process::Command;

/// Command-line arguments for the QuadPlayer application.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Play a 2x2 mosaic of local videos", long_about = None)]
pub struct Args {
    #[arg(num_args = 0.., default_value = ".")]
    pub inputs: Vec<String>,

    #[arg(long)]
    pub width: Option<i32>,

    #[arg(long)]
    pub height: Option<i32>,

    #[arg(long, default_value_t = 0.0)]
    pub clip_seconds: f64,

    #[arg(long)]
    pub seed: Option<u64>,

    #[arg(long)]
    pub recursive: bool,

    #[arg(long)]
    pub no_shuffle: bool,

    #[arg(long, help = "Start in full-screen mode")]
    pub fullscreen: bool,

    #[arg(long, value_enum, default_value_t = TransitionMode::Fade)]
    pub transition: TransitionMode,

    #[arg(
        long,
        default_value_t = 300,
        help = "Transition duration in milliseconds"
    )]
    pub transition_ms: u64,
}

/// The visual transition style to use when swapping videos.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum TransitionMode {
    /// Instant cut with no interpolation.
    Cut,
    /// Smooth crossfade between the old and new frames.
    Fade,
}

impl TransitionMode {
    pub fn next(self) -> Self {
        match self {
            Self::Cut => Self::Fade,
            Self::Fade => Self::Cut,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cut => "CUT",
            Self::Fade => "FADE",
        }
    }
}

/// Determines the target window dimensions based on CLI arguments and display detection.
pub fn resolve_dimensions(args: &Args) -> Result<(i32, i32)> {
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
pub fn detect_display_dimensions() -> Result<(i32, i32)> {
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

    #[test]
    fn test_transition_mode_cycle() {
        assert_eq!(TransitionMode::Cut.next(), TransitionMode::Fade);
        assert_eq!(TransitionMode::Fade.next(), TransitionMode::Cut);
    }

    #[test]
    fn test_transition_mode_label() {
        assert_eq!(TransitionMode::Cut.label(), "CUT");
        assert_eq!(TransitionMode::Fade.label(), "FADE");
    }

    #[test]
    fn test_resolve_dimensions_defaults() {
        let args = Args {
            inputs: vec![],
            width: None,
            height: None,
            clip_seconds: 0.0,
            seed: None,
            recursive: false,
            no_shuffle: false,
            fullscreen: false,
            transition: TransitionMode::Fade,
            transition_ms: 300,
        };
        let (w, h) = resolve_dimensions(&args).unwrap();
        assert_eq!(w, 1280);
        assert_eq!(h, 720);
    }

    #[test]
    fn test_resolve_dimensions_explicit() {
        let args = Args {
            inputs: vec![],
            width: Some(1920),
            height: Some(1080),
            clip_seconds: 0.0,
            seed: None,
            recursive: false,
            no_shuffle: false,
            fullscreen: false,
            transition: TransitionMode::Cut,
            transition_ms: 0,
        };
        let (w, h) = resolve_dimensions(&args).unwrap();
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
    }
}
