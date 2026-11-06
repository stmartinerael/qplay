# Intended module contracts

Dependencies point downward: application shell -> playback policy -> selection; shell -> capture, rendering, keybindings, scene persistence and downloader. Scene editing and downloading depend on playback operations; local playback requires neither. OpenCV is confined to capture, rendering and the application shell. CLI parsing stays outside playback policy.

- Playback policy owns slot selection, pause, seek and timing decisions and transition state. It accepts commands and time values, exposes snapshots and effects, and rejects invalid slot indices or metadata without changing state. It has no CLI, OpenCV, scene or downloader dependency.
- Discovery and selection own file discovery, ordering, shuffle and exclusion. Selection returns an optional entry; an empty pool is normal. Discovery errors must be reported rather than masquerading as an empty directory.
- Capture owns OpenCV video handles and translates metadata, frame reads and seeks into results. Invalid metadata must not produce invalid ranges; failed opens must leave the previous slot intact.
- Rendering and window management own OpenCV frames, layout and presentation. Dimensions are validated before allocation. Rendering reports errors to the shell.
- Keybindings translate keys to application commands; editing input is a shell concern. Configuration loading reports malformed or inaccessible files.
- Scene persistence owns the existing JSON sidecar format and validates bounds. Failed reads or writes preserve existing data; unknown frame counts do not impose a false upper bound.
- Downloading owns subprocesses, temporary files and completion. It reports success, failure or cancellation; a stale completion must not replace a newer slot. Temporary artifacts are cleaned up predictably.
- The shell coordinates these components, presents failures and cleans up resources on exit. Playback continues when an optional operation fails.

The useful local playback subset consists of discovery, playback policy, capture, rendering, keybindings and the shell; no download or scene-editing implementation is needed for its policies or tests.
