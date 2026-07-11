//! User-editable settings, persisted to `ux0:data/xcloud-rust/settings.json` on each change.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SETTINGS_DIR: &str = "ux0:data/xcloud-rust";
const SETTINGS_PATH: &str = "ux0:data/xcloud-rust/settings.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    EnUs,
    EnGb,
    PtBr,
    PtPt,
    EsEs,
    EsMx,
    FrFr,
    DeDe,
    ItIt,
    JaJp,
    KoKr,
    ZhCn,
    ZhTw,
    RuRu,
    PlPl,
    NlNl,
    SvSe,
    TrTr,
    ArSa,
}

impl Locale {
    pub const ALL: [Locale; 19] = [
        Self::EnUs,
        Self::EnGb,
        Self::PtBr,
        Self::PtPt,
        Self::EsEs,
        Self::EsMx,
        Self::FrFr,
        Self::DeDe,
        Self::ItIt,
        Self::JaJp,
        Self::KoKr,
        Self::ZhCn,
        Self::ZhTw,
        Self::RuRu,
        Self::PlPl,
        Self::NlNl,
        Self::SvSe,
        Self::TrTr,
        Self::ArSa,
    ];

    /// `(locale code, store market, native-language label)`.
    fn info(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::EnUs => ("en-US", "US", "English (US)"),
            Self::EnGb => ("en-GB", "GB", "English (UK)"),
            Self::PtBr => ("pt-BR", "BR", "Português (Brasil)"),
            Self::PtPt => ("pt-PT", "PT", "Português (Portugal)"),
            Self::EsEs => ("es-ES", "ES", "Español (España)"),
            Self::EsMx => ("es-MX", "MX", "Español (México)"),
            Self::FrFr => ("fr-FR", "FR", "Français"),
            Self::DeDe => ("de-DE", "DE", "Deutsch"),
            Self::ItIt => ("it-IT", "IT", "Italiano"),
            Self::JaJp => ("ja-JP", "JP", "日本語"),
            Self::KoKr => ("ko-KR", "KR", "한국어"),
            Self::ZhCn => ("zh-CN", "CN", "中文（简体）"),
            Self::ZhTw => ("zh-TW", "TW", "中文（繁體）"),
            Self::RuRu => ("ru-RU", "RU", "Русский"),
            Self::PlPl => ("pl-PL", "PL", "Polski"),
            Self::NlNl => ("nl-NL", "NL", "Nederlands"),
            Self::SvSe => ("sv-SE", "SE", "Svenska"),
            Self::TrTr => ("tr-TR", "TR", "Türkçe"),
            Self::ArSa => ("ar-SA", "SA", "العربية"),
        }
    }

    /// The locale code sent to xCloud, e.g. `"pt-BR"`.
    pub fn as_str(self) -> &'static str {
        self.info().0
    }

    /// The Microsoft Store catalog's `market` query param, e.g. `"BR"`.
    pub fn market(self) -> &'static str {
        self.info().1
    }

    /// Display label in the language's own native name, e.g. `"Русский"`.
    pub fn label(self) -> &'static str {
        self.info().2
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub locale: Locale,
    /// Shows internal stream/session state on the `Streaming` screen. Off by default.
    pub show_stream_debug_info: bool,
    pub game_profiles: HashMap<String, GameProfile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameProfile {
    pub swap_shoulders_and_triggers: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: Locale::default(),
            show_stream_debug_info: false,
            game_profiles: HashMap::new(),
        }
    }
}

impl Settings {
    pub fn game_profile(&self, title_id: &str) -> Option<&GameProfile> {
        self.game_profiles.get(title_id)
    }

    pub fn set_swap_shoulders_and_triggers(&mut self, title_id: String, enabled: bool) {
        self.game_profiles
            .entry(title_id)
            .or_default()
            .swap_shoulders_and_triggers = enabled;
    }

    /// Loads from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        let data = match std::fs::read_to_string(SETTINGS_PATH) {
            Ok(data) => data,
            Err(error) => {
                eprintln!("Settings: no existing {SETTINGS_PATH} ({error}), using defaults");
                return Self::default();
            }
        };
        match serde_json::from_str(&data) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("Settings: failed to parse {SETTINGS_PATH} ({error}), using defaults");
                Self::default()
            }
        }
    }

    /// Saves to disk; errors are logged rather than surfaced.
    pub fn save(&self) {
        let result = std::fs::create_dir_all(SETTINGS_DIR)
            .context("failed to create settings directory")
            .and_then(|_| {
                serde_json::to_string_pretty(self).context("failed to serialize settings")
            })
            .and_then(|data| crate::fs_utils::write_file_truncating(SETTINGS_PATH, data));

        if let Err(error) = result {
            eprintln!("Settings: failed to save: {error:#}");
        }
    }
}
