# Module boundaries

- `playback` owns the selection pool, seeded shuffle and clip-start selection, slot identities, displayed-frame positions, exclusive clip ends, pause state, transition policy, and scheduling. Commands accept explicit times and produce backend-independent open, seek, read, and replacement effects. Capture operation results carry per-slot identities; failed or stale results do not commit metadata. Read-only slot snapshots drive rendering.
- `video` discovers files and reports traversal errors. `VideoPool`, owned by playback, handles queue ordering, shuffle and exclusion; if every entry is in use it permits repetition.
- `capture` owns the current and candidate OpenCV decoder handles, indexed by slot. A candidate is committed only after its first frame is decoded and playback accepts the metadata. Only existing local files are opened; remote source strings are not passed to OpenCV. A failed seek attempts to restore the next decoder position and explicitly reports restoration failure.
- `ui` owns the OpenCV window, current and transition frames, and mosaic composition. The coordinator supplies labels, playback snapshots and display options and receives keys.
- `controls` maps configured keys to application actions. Text entry remains in the coordinator. Configuration errors are reported and defaults are used.
- `scenes` (feature `scene-editing`) uses the existing JSON sidecar format, validates bounds, and writes via a temporary file and rename. Failed reads or writes do not replace existing data.
- `player` coordinates these components, translates input, executes effects, and presents operation failures in the terminal and affected tile. A failed empty slot is not retried each tick.

Scene editing is enabled by default. Local playback builds without scene editing using `--no-default-features`.
