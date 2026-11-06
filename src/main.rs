//! QuadPlayer: A 2x2 mosaic video player using OpenCV.
//!
//! This application searches for video files in a specified directory
//! and plays four of them simultaneously in a 2x2 grid. It features smooth transitions,
//! random shuffling, and interactive playback controls.

use anyhow::Result;
use clap::Parser;
use std::path::{Path, PathBuf};

pub mod cli;
pub mod config;
pub mod controls;
pub mod downloader;
pub mod playback;
pub mod player;
pub mod scenes;
pub mod ui;
pub mod video;

use cli::{Args, resolve_dimensions};
use player::QuadPlayer;
use video::{VideoEntry, discover_videos};

fn main() -> Result<()> {
    let args = Args::parse();
    let config = match config::load_keybindings() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Could not load keybindings: {error:#}; using defaults");
            config::Keybindings::default()
        }
    };

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
            .map(|s| VideoEntry::new(PathBuf::from(s)))
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
