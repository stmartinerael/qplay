use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Scene {
    pub start_frame: i32,
    pub end_frame: i32,
    pub tags: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SceneData {
    pub scenes: Vec<Scene>,
}

pub fn save_scenes(video_path: &Path, data: &SceneData, total_frames: i32) -> Result<()> {
    // Check invariants
    for scene in &data.scenes {
        if scene.start_frame < 0
            || scene.start_frame > scene.end_frame
            || scene.end_frame > total_frames
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
    fs::write(sidecar_path, json).context("Failed to write scene data")?;
    Ok(())
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
