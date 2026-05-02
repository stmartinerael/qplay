# qplay

`qplay` is a Rust-based CLI application that plays a 2x2 mosaic of local videos using OpenCV. It features a modern UI with a Dracula-inspired color palette, smooth transitions, and interactive controls.

## Project Overview

- **Core Functionality:** Recursively (optional) discovers video files in a directory and plays four of them simultaneously in a grid.
- **Tech Stack:**
  - **Language:** Rust (Edition 2024)
  - **Graphics/Video:** `opencv` (v0.94)
  - **CLI Parsing:** `clap` (v4.5) with `derive`
  - **Utilities:** `rand` (shuffling), `walkdir` (file discovery), `anyhow` (error handling)
- **Architecture:**
  - `QuadPlayer`: The main orchestrator managing the window and playback loop.
  - `VideoPool`: Handles video selection, shuffling, and ensuring unique videos are played when possible.
  - `SlotState`: Manages the playback state (capture, frame index, timing) for each of the four tiles.
  - `Dracula`: Custom theme constants for BGR colors used in overlays and UI elements.

## Building and Running

### Prerequisites
- Rust toolchain (latest stable recommended)
- OpenCV libraries installed on the system (required by the `opencv` crate)

### Commands
- **Build:** `cargo build`
- **Run:** `cargo run -- [DIRECTORY] [FLAGS]`
  - Example: `cargo run -- ~/Videos --recursive --clip-seconds 10`
- **Test:** `cargo test`
- **Lint:** `cargo clippy`
- **Format:** `cargo fmt`

### CLI Arguments
- `directory`: The root directory to search for videos (default: `.`).
- `--recursive`: Search subdirectories for videos.
- `--width`, `--height`: Set the window dimensions (default: 1280x720).
- `--clip-seconds`: If set, play only a random segment of this length from each video.
- `--no-shuffle`: Disable random shuffling of the video pool.
- `--fullscreen`: Start in full-screen mode.
- `--transition`: Transition mode (`fade` or `cut`, default: `fade`).
- `--transition-ms`: Duration of the fade transition in milliseconds (default: 300).

## Development Conventions

### Workflow
- **Branching:** Use `feature/*` or `bugfix/*` branches. Avoid direct commits to `main`.
- **Commits:** Make frequent, intentional commits.
- **Validation:** Run `cargo test` and ensure code is formatted before merging.

### Rust Style
- **Formatting:** `rustfmt` is mandatory. A Git pre-commit hook is provided in `.githooks/pre-commit` to automate this.
- **Error Handling:** Use `anyhow::Result` for application-level logic.
- **UI:** UI elements are rendered directly onto OpenCV `Mat` objects using `imgproc` functions.

### UI Controls (Interactive)
- `q` / `Esc`: Quit application.
- `Space`: Pause/Resume playback.
- `1` - `4`: Immediately swap the video in the corresponding slot.
- `c`: Swap videos in all slots.
- `r`: Reshuffle the video pool.
- `t`: Cycle through transition modes (Fade/Cut).
- `f`: Toggle fullscreen mode.
- `h`: Toggle the help overlay.
