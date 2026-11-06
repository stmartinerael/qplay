use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub start_frame: i32,
    pub end_frame: i32,
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SceneData {
    pub scenes: Vec<Scene>,
}

pub fn save_scenes(video_path: &Path, data: &SceneData, total_frames: i32) -> Result<()> {
    // Check invariants
    for scene in &data.scenes {
        if scene.start_frame < 0
            || scene.start_frame > scene.end_frame
            || (total_frames > 0 && scene.end_frame > total_frames)
        {
            anyhow::bail!(
                "Scene invariant violated: 0 <= {} <= {} <= {}",
                scene.start_frame,
                scene.end_frame,
                total_frames
            );
        }
    }

    let sidecar_path = get_sidecar_path(video_path);
    let json = serde_json::to_string_pretty(data)?;
    // Write beside the destination so rename is atomic on the same filesystem.
    let temp_path = sidecar_path.with_file_name(format!(
        ".{}.{}.tmp",
        sidecar_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        rand::random::<u64>()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .context("Failed to create temporary scene data")?;
    let result = (|| -> Result<()> {
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp_path, &sidecar_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result.context("Failed to write scene data")
}

pub fn load_scenes(video_path: &Path) -> Result<SceneData> {
    let sidecar_path = get_sidecar_path(video_path);
    if sidecar_path.exists() {
        let contents = fs::read_to_string(&sidecar_path)?;
        let data = serde_json::from_str(&contents)?;
        Ok(data)
    } else {
        Ok(SceneData::default())
    }
}

fn get_sidecar_path(video_path: &Path) -> PathBuf {
    let mut path = video_path.to_path_buf();
    let mut file_name = path.file_name().unwrap_or_default().to_os_string();
    file_name.push(".scenes.json");
    path.set_file_name(file_name);
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_save_and_load_scenes() -> Result<()> {
        let dir = tempdir()?;
        let video_path = dir.path().join("test_video.mp4");
        std::fs::File::create(&video_path)?;

        let mut data = SceneData::default();
        data.scenes.push(Scene {
            start_frame: 10,
            end_frame: 100,
            tags: vec!["intro".to_string(), "action".to_string()],
        });

        save_scenes(&video_path, &data, 200)?;

        let loaded = load_scenes(&video_path)?;
        assert_eq!(loaded, data);
        Ok(())
    }

    #[test]
    fn test_save_scenes_invariants() {
        let path = Path::new("/fake/video.mp4");
        let mut data = SceneData::default();

        // start < 0
        data.scenes.push(Scene {
            start_frame: -1,
            end_frame: 10,
            tags: vec![],
        });
        assert!(save_scenes(path, &data, 100).is_err());

        // start > end
        data.scenes.clear();
        data.scenes.push(Scene {
            start_frame: 50,
            end_frame: 20,
            tags: vec![],
        });
        assert!(save_scenes(path, &data, 100).is_err());

        // end > total_frames
        data.scenes.clear();
        data.scenes.push(Scene {
            start_frame: 0,
            end_frame: 150,
            tags: vec![],
        });
        assert!(save_scenes(path, &data, 100).is_err());
    }

    #[test]
    fn invalid_update_keeps_existing_sidecar() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("video.mp4");
        let original = SceneData {
            scenes: vec![Scene {
                start_frame: 1,
                end_frame: 5,
                tags: vec![],
            }],
        };
        save_scenes(&path, &original, 10)?;
        let invalid = SceneData {
            scenes: vec![Scene {
                start_frame: 8,
                end_frame: 2,
                tags: vec![],
            }],
        };
        assert!(save_scenes(&path, &invalid, 10).is_err());
        assert_eq!(load_scenes(&path)?, original);
        Ok(())
    }

    #[test]
    fn unknown_frame_count_allows_valid_scenes() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("video.mp4");
        let data = SceneData {
            scenes: vec![Scene {
                start_frame: 2,
                end_frame: 10,
                tags: vec![],
            }],
        };
        save_scenes(&path, &data, 0)?;
        assert_eq!(load_scenes(&path)?, data);
        Ok(())
    }

    #[test]
    fn malformed_sidecar_is_not_silently_loaded() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("video.mp4");
        let sidecar = get_sidecar_path(&path);
        std::fs::write(&sidecar, "broken")?;
        assert!(load_scenes(&path).is_err());
        assert_eq!(std::fs::read_to_string(sidecar)?, "broken");
        Ok(())
    }

    #[test]
    fn test_load_scenes_nonexistent() -> Result<()> {
        let path = Path::new("/nonexistent/video.mp4");
        let data = load_scenes(path)?;
        assert!(data.scenes.is_empty());
        Ok(())
    }
}
