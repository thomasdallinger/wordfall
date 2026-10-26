use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// Languages the user dictates in (ISO 639-1). Empty = any language whisper knows;
    /// several = each dictation is decoded in whichever of them it sounds like.
    pub languages: Vec<String>,
    pub remove_fillers: bool,
    /// Turn spoken enumerations ("first … second …", "bullet point …") into lists.
    pub format_lists: bool,
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
            languages: default_languages(),
            remove_fillers: true,
            format_lists: true,
            // Windows never sees the Fn key; Right Ctrl has no other job there (Right Alt is AltGr).
            ptt_key: if cfg!(windows) { PttKey::RightControl } else { PttKey::Fn },
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
            own: own_models_dir(data_dir),
            extra: self.model_folders.iter().map(PathBuf::from).collect(),
        }
    }

    pub fn load(dir: &Path) -> Self {
        let Some(mut raw) = storage::read_json::<serde_json::Value>(&dir.join("settings.json")) else {
            return Self::default();
        };
        migrate(&mut raw);
        serde_json::from_value(raw).unwrap_or_else(|e| {
            log::warn!("could not read settings: {e}");
            Self::default()
        })
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
        let mut seen = std::collections::HashSet::new();
        next.languages.retain(|l| whisper_rs::get_lang_id(l).is_some() && seen.insert(l.clone()));
        Ok(next)
    }
}

/// English plus the system language, if whisper knows it (e.g. ["en", "de"] on a German Mac).
fn default_languages() -> Vec<String> {
    let mut langs = vec!["en".to_string()];
    let system = sys_locale::get_locale().and_then(|l| l.split(['-', '_']).next().map(str::to_lowercase));
    if let Some(code) = system.filter(|c| c != "en" && whisper_rs::get_lang_id(c).is_some()) {
        langs.push(code);
    }
    langs
}

/// Upgrades settings written by older versions.
fn migrate(raw: &mut serde_json::Value) {
    let Some(obj) = raw.as_object_mut() else { return };
    // `language` (one code, or null for automatic) became `languages`. "en" was the old
    // default rather than a choice, so it gets the new default that includes the system language.
    if let Some(old) = obj.remove("language") {
        if !obj.contains_key("languages") {
            match old.as_str() {
                Some("en") => {}
                Some(code) => {
                    obj.insert("languages".into(), serde_json::json!([code]));
                }
                None => {
                    obj.insert("languages".into(), serde_json::json!([]));
                }
            }
        }
    }
}

/// Models are large; on Windows they belong in the local (not roaming) app data folder.
fn own_models_dir(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    if let (Some(roaming), Some(local)) = (dirs::data_dir(), dirs::data_local_dir()) {
        if let Ok(rel) = data_dir.strip_prefix(&roaming) {
            return local.join(rel).join("models");
        }
    }
    data_dir.join("models")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn migrated(v: serde_json::Value) -> Settings {
        let mut v = v;
        migrate(&mut v);
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn migrates_single_language() {
        assert_eq!(migrated(json!({ "language": "de" })).languages, ["de"]);
    }

    #[test]
    fn migrates_automatic_language() {
        assert!(migrated(json!({ "language": null })).languages.is_empty());
    }

    #[test]
    fn old_english_default_gets_new_default() {
        assert_eq!(migrated(json!({ "language": "en" })).languages, default_languages());
    }

    #[test]
    fn keeps_new_languages_field() {
        assert_eq!(migrated(json!({ "language": "fr", "languages": ["en", "de"] })).languages, ["en", "de"]);
    }
}
