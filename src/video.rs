use rand::SeedableRng;
use rand::seq::SliceRandom;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Supported video file extensions.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "avi", "webm", "mpg", "mpeg"];

/// Represents a single video file discovered in the target directory or passed explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoEntry {
    /// The absolute or relative path to the video file.
    pub path: PathBuf,
}

impl VideoEntry {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns the file name for UI display.
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

/// Searches a directory for supported video files.
///
/// Filters out partial downloads (e.g., `.part` files) and limits by extension.
/// If `recursive` is true, subdirectories are also searched.
pub fn discover_videos(directory: &Path, recursive: bool) -> anyhow::Result<Vec<VideoEntry>> {
    let mut entries = Vec::new();
    let walker = if recursive {
        WalkDir::new(directory)
    } else {
        WalkDir::new(directory).max_depth(1)
    };

    for entry in walker {
        let entry = entry?;
        if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            if let Some(ext) = path.extension().and_then(|s| s.to_str())
                && VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
                && !path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .ends_with(".part")
            {
                entries.push(VideoEntry::new(path));
            }
        }
    }
    entries.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
    Ok(entries)
}

/// Manages a collection of discovered videos, handling shuffling and selection
/// to ensure diverse playback without immediate repetition.
pub struct VideoPool {
    entries: Vec<VideoEntry>,
    shuffle: bool,
    rng: rand::rngs::StdRng,
    queue: Vec<VideoEntry>,
}

impl VideoPool {
    pub fn new(entries: Vec<VideoEntry>, shuffle: bool, seed: Option<u64>) -> Self {
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

    pub fn refill_queue(&mut self) {
        self.queue = self.entries.clone();
        if self.shuffle {
            self.queue.shuffle(&mut self.rng);
        }
    }

    pub fn next_entry(&mut self, exclude: &[PathBuf]) -> Option<VideoEntry> {
        if self.queue.is_empty() {
            self.refill_queue();
        }

        if self.queue.is_empty() {
            return None;
        }

        for i in 0..self.queue.len() {
            if !exclude.contains(&self.queue[i].path) || exclude.len() >= self.entries.len() {
                return Some(self.queue.remove(i));
            }
        }

        Some(self.queue.remove(0))
    }

    pub fn reshuffle(&mut self) {
        self.refill_queue();
    }

    pub fn total_entries(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[VideoEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn test_video_entry_display_name() {
        let entry = VideoEntry::new(PathBuf::from("/path/to/movie.mp4"));
        assert_eq!(entry.display_name(), "movie.mp4");
    }

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
        let entries = discover_videos(path, false)?;
        assert_eq!(entries.len(), 2);
        let names: Vec<_> = entries.iter().map(|e| e.display_name()).collect();
        assert!(names.contains(&"video1.mp4".to_string()));
        assert!(names.contains(&"video2.MOV".to_string()));

        // Test recursive
        let entries_rec = discover_videos(path, true)?;
        assert_eq!(entries_rec.len(), 3);

        Ok(())
    }

    #[test]
    fn discovery_reports_missing_directory() {
        assert!(discover_videos(Path::new("/nonexistent/qplay-test-directory"), true).is_err());
    }

    #[test]
    fn test_video_pool_logic() {
        let entries = vec![
            VideoEntry::new(PathBuf::from("a.mp4")),
            VideoEntry::new(PathBuf::from("b.mp4")),
            VideoEntry::new(PathBuf::from("c.mp4")),
        ];

        let mut pool = VideoPool::new(entries.clone(), false, Some(42));

        // Initial queue should match entries when not shuffled
        assert_eq!(pool.queue.len(), 3);
        assert_eq!(pool.queue[0].path, PathBuf::from("a.mp4"));

        // Test next_entry with exclusion
        let exclude = vec![PathBuf::from("a.mp4")];
        let next = pool.next_entry(&exclude).unwrap();
        assert_eq!(next.path, PathBuf::from("b.mp4"));
        assert_eq!(pool.queue.len(), 2);

        // Test exhaustion and refill
        pool.next_entry(&[]); // gets 'a.mp4'
        assert_eq!(pool.queue.len(), 1);

        let mut pool2 = VideoPool::new(entries, false, None);
        let _ = pool2.next_entry(&[]); // a
        let _ = pool2.next_entry(&[]); // b
        let _ = pool2.next_entry(&[]); // c
        assert!(pool2.queue.is_empty());

        let next_refill = pool2.next_entry(&[]).unwrap();
        assert_eq!(next_refill.path, PathBuf::from("a.mp4"));
        assert_eq!(pool2.queue.len(), 2);
    }

    #[test]
    fn selection_repeats_when_every_entry_is_excluded() {
        let mut pool = VideoPool::new(
            vec![VideoEntry::new(PathBuf::from("one.mp4"))],
            false,
            Some(1),
        );
        assert_eq!(
            pool.next_entry(&[PathBuf::from("one.mp4")]).unwrap().path,
            PathBuf::from("one.mp4")
        );
    }

    #[test]
    fn empty_pool_returns_none() {
        let mut pool = VideoPool::new(vec![], false, None);
        assert!(pool.next_entry(&[]).is_none());
    }

    #[test]
    fn test_video_pool_shuffle() {
        let entries = (0..10)
            .map(|i| VideoEntry::new(PathBuf::from(format!("{}.mp4", i))))
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
