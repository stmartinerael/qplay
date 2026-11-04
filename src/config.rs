use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Keybindings {
    pub select_slot_1: char,
    pub select_slot_2: char,
    pub select_slot_3: char,
    pub select_slot_4: char,
    pub seek_forward_active: char,
    pub seek_backward_active: char,
    pub pause_active: char,
    pub load_video: char,
    pub mark_scene_start: char,
    pub mark_scene_stop: char,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            select_slot_1: '!',        // Shift+1
            select_slot_2: '@',        // Shift+2
            select_slot_3: '#',        // Shift+3
            select_slot_4: '$',        // Shift+4
            seek_forward_active: '.',  // > without shift
            seek_backward_active: ',', // < without shift
            pause_active: 'k',
            load_video: 'l',
            mark_scene_start: '[',
            mark_scene_stop: ']',
        }
    }
}

pub fn load_keybindings() -> Result<Keybindings> {
    let proj_dirs =
        ProjectDirs::from("", "", "qplay").context("Could not determine config directory")?;
    let config_dir = proj_dirs.config_dir();
    let config_path = config_dir.join("keybindings.json");

    if config_path.exists() {
        let contents = fs::read_to_string(&config_path)?;
        let bindings = serde_json::from_str(&contents)?;
        Ok(bindings)
    } else {
        let bindings = Keybindings::default();
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        let json = serde_json::to_string_pretty(&bindings)?;
        fs::write(&config_path, json)?;
        Ok(bindings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keybindings_default_roundtrip() -> Result<()> {
        let default_bindings = Keybindings::default();
        let json = serde_json::to_string_pretty(&default_bindings)?;
        let deserialized: Keybindings = serde_json::from_str(&json)?;
        assert_eq!(default_bindings, deserialized);
        Ok(())
    }

    #[test]
    fn test_keybindings_defaults_values() {
        let bindings = Keybindings::default();
        assert_eq!(bindings.select_slot_1, '!');
        assert_eq!(bindings.pause_active, 'k');
        assert_eq!(bindings.load_video, 'l');
        assert_eq!(bindings.mark_scene_start, '[');
        assert_eq!(bindings.mark_scene_stop, ']');
    }
}
