# qplay

qplay plays local videos in a 2×2 OpenCV window. It can select videos from a directory or accept explicit paths.

## Build and run

Install Rust and OpenCV development libraries, then run:

```sh
cargo run -- ~/Videos --recursive
cargo run -- video1.mp4 video2.mp4 --no-shuffle
cargo run -- --help
```

The default window is 1280×720. `--width` and `--height` must be at least 240 and 160 respectively. Use `--fullscreen`, `--clip-seconds`, `--seed`, `--transition cut|fade`, and `--transition-ms` to adjust playback. `--clip-seconds` must be finite and non-negative. URL downloads require `yt-dlp` on PATH.

## Controls

| Key | Action |
| --- | --- |
| q or Esc | Quit |
| Space | Pause or resume all slots |
| 1–4 | Replace the corresponding slot |
| !, @, #, $ | Select a slot |
| k | Pause the selected slot |
| , / . | Seek the selected slot backward / forward five seconds |
| l | Enter a local path or URL; Enter submits, Esc cancels |
| [ / ] | Mark scene start / end; enter tags after the end mark |
| c / r | Replace all slots / reshuffle |
| t / f / h | Change transition / fullscreen / help |

Customizable keys are stored in the platform configuration directory under `qplay/keybindings.json`. Scene annotations are stored beside the video as `<filename>.scenes.json`. Malformed scene data is reported and is not replaced by an empty scene list. Playback operation errors are printed to the terminal.

## Development

Run `cargo fmt --check`, `cargo test`, and `cargo clippy`. The intended boundaries and their contracts are described in [docs/module-contracts.md](docs/module-contracts.md); the refactor toward these boundaries is ongoing. Develop on a feature or bugfix branch and commit tested stages. The supplied `.githooks/pre-commit` runs rustfmt on staged Rust files when configured as the Git hooks path.
