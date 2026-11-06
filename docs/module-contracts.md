# Module boundaries

- `playback` owns selected and paused slots, displayed-frame positions, clip bounds, frame scheduling, and transition timing. It accepts explicit times and exposes snapshots and frame actions without depending on OpenCV or CLI types. The coordinator commits an open or seek only after decoding succeeds. Slot paths and selection from the video pool remain in the coordinator.
- `video` discovers files and reports traversal errors; `VideoPool` handles ordering, shuffle and exclusion. An empty selection is represented by `None`.
- `capture` owns OpenCV video handles and exposes metadata, reads and seeks. Opening a replacement validates its first frame before replacing a working slot. After a failed seek the coordinator attempts to restore the decoder's next-frame position; backend failure can prevent that restoration.
- `ui` owns the OpenCV window lifecycle, mosaic composition, and frame rendering. The coordinator supplies rendering information and receives keys.
- `controls` maps configured keys to application actions. Text entry remains in the coordinator. Configuration errors are reported and defaults are used.
- `scenes` (feature `scene-editing`) uses the existing JSON sidecar format, validates bounds, and writes via a temporary file and rename. Failed reads or writes do not replace existing data.
- `downloader` (feature `downloading`) owns the child process, worker and temporary directory. Dropping a job cancels it, waits for the worker to kill and reap the child, and cleans up its temporary files. Each slot owns at most one pending job; replacing it drops the old job before accepting another.
- `player` coordinates these components and presents operation failures in the terminal and affected tile. Its capture/frame ownership and some replacement and transition policy remain in the coordinator; the intended command/effect separation is not yet complete.

Both optional features are enabled by default. Local playback builds without either using `--no-default-features`.
