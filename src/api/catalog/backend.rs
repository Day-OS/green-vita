use super::{Game, GameDetails, cache};
use crate::api_geforce_now::auth::NvidiaAuth;
use crate::api_geforce_now::catalog as geforce_now_catalog;
use crate::api_xbox::api::ApiClient;
use crate::api_xbox::game_catalog;
use anyhow::Result;
use reqwest::Client;

/// Extension point for loading game libraries and provider-specific metadata.
#[derive(Clone)]
pub enum GameCatalogBackend {
    XCloud { api: ApiClient },
    GeForceNow { auth: NvidiaAuth },
}

impl GameCatalogBackend {
    pub fn xcloud(api: ApiClient) -> Self {
        Self::XCloud { api }
    }

    pub fn geforce_now(auth: NvidiaAuth) -> Self {
        Self::GeForceNow { auth }
    }

    pub fn supports_streaming(&self) -> bool {
        matches!(self, Self::XCloud { .. })
    }

    /// Stable namespace used to keep cache entries from different providers apart.
    pub fn cache_namespace(&self) -> &'static str {
        match self {
            Self::XCloud { .. } => "xcloud",
            Self::GeForceNow { .. } => "geforce-now",
        }
    }

    pub async fn load_games(&self) -> Result<Vec<Game>> {
        let result = match self {
            Self::XCloud { api } => game_catalog::load_games(api).await,
            Self::GeForceNow { auth } => geforce_now_catalog::load_games(auth).await,
        };

        match result {
            Ok(games) => {
                if let Err(error) = save_cached_games(self.cache_namespace(), &games) {
                    eprintln!("Game catalog: failed to cache game list: {error:#}");
                }
                Ok(games)
            }
            Err(error) => match load_cached_games(self.cache_namespace()) {
                Some(games) => {
                    eprintln!(
                        "Game catalog: using cached game list after request failed: {error:#}"
                    );
                    Ok(games)
                }
                None => Err(error),
            },
        }
    }

    pub(crate) async fn fetch_details(
        &self,
        client: &Client,
        metadata_id: &str,
        market: &str,
        language: &str,
    ) -> Result<GameDetails> {
        match self {
            Self::XCloud { .. } => {
                game_catalog::fetch_details(client, metadata_id, market, language).await
            }
            Self::GeForceNow { .. } => {
                anyhow::bail!("GeForce NOW games already include their catalog metadata")
            }
        }
    }
}

fn load_cached_games(namespace: &str) -> Option<Vec<Game>> {
    let path = cache::provider_path(namespace, "games.json");
    let bytes = cache::read(&path)?;
    match serde_json::from_slice(&bytes) {
        Ok(games) => Some(games),
        Err(error) => {
            eprintln!("Game catalog: invalid cached game list: {error}");
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn save_cached_games(namespace: &str, games: &[Game]) -> Result<()> {
    let path = cache::provider_path(namespace, "games.json");
    let bytes = serde_json::to_vec(games)?;
    cache::write(&path, bytes)
}
