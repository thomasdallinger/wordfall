use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{
    storage,
    transcribe::{DecodeOptions, Vad},
    vocab::PromptMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PttKey {
    Fn,
    RightOption,
    RightCommand,
    RightControl,
    RightShift,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetVisibility {
    Hidden,
    Recording,
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub model: String,
    pub language: Option<String>,
    pub remove_fillers: bool,
    pub ptt_key: PttKey,
    pub toggle_shortcut: Option<String>,
    pub show_widget_shortcut: String,
    pub input_device: Option<String>,
    pub input_gain: f32,
    pub sounds_enabled: bool,
    pub mute_system_audio: bool,
    pub copy_only: bool,
    pub launch_at_login: bool,
    pub widget_visibility: WidgetVisibility,
    pub pill_style: String,
    pub follow_cursor: bool,
    pub widget_locked: bool,
    pub dictionary_enabled: bool,
    pub onboarding_done: bool,
    /// Extra folders searched for Whisper models.
    pub model_folders: Vec<String>,
    /// Keep a WAV of each dictation so it can be corrected and used as benchmark data.
    pub keep_audio: bool,
    /// Vocabulary hints fed to the recognizer (see `vocab.rs`).
    pub prompt_mode: PromptMode,
    /// Advanced decoder settings (tuned with `wordfall bench`).
    pub decode: DecodeOptions,
    /// Saved widget position (top-left, physical px) when the user dragged it.
    pub widget_position: Option<(i32, i32)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Defaults chosen with `wordfall bench` (bench/configs/bakeoff.toml, 2026-10-20).
            model: "large-v3-turbo-q8_0".into(),
            language: Some("en".into()),
            remove_fillers: true,
            ptt_key: PttKey::Fn,
            toggle_shortcut: None,
            show_widget_shortcut: "CommandOrControl+Shift+Space".into(),
            input_device: None,
            input_gain: 1.0,
            sounds_enabled: true,
            mute_system_audio: false,
            copy_only: false,
            launch_at_login: true,
            widget_visibility: WidgetVisibility::Always,
            pill_style: "logo-and-text".into(),
            follow_cursor: true,
            widget_locked: false,
            dictionary_enabled: true,
            onboarding_done: false,
            model_folders: Vec::new(),
            keep_audio: false,
            prompt_mode: PromptMode::None,
            decode: DecodeOptions { vad: Vad::Silero, ..DecodeOptions::default() },
            widget_position: None,
        }
    }
}

impl Settings {
    pub fn model_dirs(&self, data_dir: &Path) -> crate::transcribe::ModelDirs {
        crate::transcribe::ModelDirs {
            own: data_dir.join("models"),
            extra: self.model_folders.iter().map(std::path::PathBuf::from).collect(),
        }
    }

    pub fn load(dir: &Path) -> Self {
        storage::read_json(&dir.join("settings.json")).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        storage::write_json(&dir.join("settings.json"), self)
    }

    /// Apply a partial JSON patch (camelCase keys) on top of the current settings.
    pub fn patched(&self, patch: serde_json::Value) -> anyhow::Result<Self> {
        let mut current = serde_json::to_value(self)?;
        if let (Some(obj), Some(p)) = (current.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                obj.insert(k.clone(), v.clone());
            }
        }
        let mut next: Settings = serde_json::from_value(current)?;
        next.input_gain = next.input_gain.clamp(0.5, 4.0);
        Ok(next)
    }
}
