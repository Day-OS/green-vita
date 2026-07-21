mod backend;
mod cache;
pub mod worker;

pub use backend::GameCatalogBackend;

use crate::app::TitleImage;
use std::sync::Arc;

/// Provider-neutral game shown by the library UI.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Game {
    /// Stable identifier used by the UI, cache and per-game settings.
    pub id: String,
    /// Opaque identifier passed back to the provider when starting the game.
    pub launch_id: String,
    /// Opaque identifier used when the provider loads additional metadata.
    pub metadata_id: Option<String>,
    pub details: Option<GameDetails>,
    #[serde(skip)]
    pub icon: Option<Arc<TitleImage>>,
    #[serde(skip)]
    pub box_art: Option<Arc<TitleImage>>,
    #[serde(skip)]
    pub background: Option<Arc<TitleImage>>,
}

impl Game {
    pub fn display_name(&self) -> &str {
        self.details
            .as_ref()
            .and_then(|details| details.name.as_deref())
            .unwrap_or(&self.id)
    }
}

/// Metadata shape understood by the shared game-library UI.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GameDetails {
    pub name: Option<String>,
    pub description: Option<String>,
    pub box_art_url: Option<String>,
    pub background_url: Option<String>,
    pub icon_url: Option<String>,
    pub genres: Vec<String>,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub release_date: Option<String>,
    pub average_rating: Option<f32>,
    pub rating_count: Option<u64>,
    pub content_rating: Option<String>,
}
