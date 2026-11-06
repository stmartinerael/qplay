//! Maps configured keys to application actions without handling input or playback.
use crate::config::Keybindings;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Select(usize),
    ToggleSlotPause,
    Seek(i32),
    Load,
    SceneStart,
    SceneStop,
    Quit,
    TogglePause,
    Replace(usize),
    ReplaceAll,
    Reshuffle,
    Help,
    Fullscreen,
    Transition,
}

pub fn translate(key: char, bindings: &Keybindings) -> Option<Action> {
    let selected = [
        bindings.select_slot_1,
        bindings.select_slot_2,
        bindings.select_slot_3,
        bindings.select_slot_4,
    ];
    if let Some(index) = selected.iter().position(|&candidate| candidate == key) {
        return Some(Action::Select(index));
    }
    for (candidate, action) in [
        (bindings.pause_active, Action::ToggleSlotPause),
        (bindings.seek_forward_active, Action::Seek(5)),
        (bindings.seek_backward_active, Action::Seek(-5)),
        (bindings.load_video, Action::Load),
    ] {
        if key == candidate {
            return Some(action);
        }
    }
    if cfg!(feature = "scene-editing") {
        if key == bindings.mark_scene_start {
            return Some(Action::SceneStart);
        }
        if key == bindings.mark_scene_stop {
            return Some(Action::SceneStop);
        }
    }
    match key {
        'q' | '\u{1b}' => Some(Action::Quit),
        ' ' => Some(Action::TogglePause),
        '1'..='4' => Some(Action::Replace(key as usize - '1' as usize)),
        'c' => Some(Action::ReplaceAll),
        'r' => Some(Action::Reshuffle),
        'h' => Some(Action::Help),
        'f' => Some(Action::Fullscreen),
        't' => Some(Action::Transition),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_controls_follow_feature_flag() {
        let keys = Keybindings::default();
        assert_eq!(
            translate(keys.mark_scene_start, &keys).is_some(),
            cfg!(feature = "scene-editing")
        );
        assert_eq!(
            translate(keys.mark_scene_stop, &keys).is_some(),
            cfg!(feature = "scene-editing")
        );
    }

    #[test]
    fn configured_keys_take_priority_over_fixed_actions() {
        let mut keys = Keybindings::default();
        assert_eq!(translate('!', &keys), Some(Action::Select(0)));
        assert_eq!(translate('.', &keys), Some(Action::Seek(5)));
        keys.pause_active = 'q';
        assert_eq!(translate('q', &keys), Some(Action::ToggleSlotPause));
        assert_eq!(translate('2', &keys), Some(Action::Replace(1)));
        assert_eq!(translate('~', &keys), None);
    }
}
