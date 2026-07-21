//! Xbox-specific adapter for the provider-neutral game catalog.

use crate::api::catalog::{Game, GameDetails};
use crate::api_xbox::api::ApiClient;
use crate::api_xbox::catalog;
use anyhow::Result;
use reqwest::Client;

pub async fn load_games(api: &ApiClient) -> Result<Vec<Game>> {
    let response = api.get_titles().await?;
    Ok(extract_games(&response))
}

pub async fn fetch_details(
    client: &Client,
    metadata_id: &str,
    market: &str,
    language: &str,
) -> Result<GameDetails> {
    let details = catalog::fetch_title_details(client, metadata_id, market, language).await?;
    Ok(GameDetails {
        name: details.name,
        description: details.description,
        box_art_url: details.box_art_url,
        background_url: details.background_url,
        icon_url: details.icon_url,
        genres: details.genres,
        developer: details.developer,
        publisher: details.publisher,
        release_date: details.release_date,
        average_rating: details.average_rating,
        rating_count: details.rating_count,
        content_rating: details.content_rating,
    })
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TitlesResponse {
    #[serde(default)]
    results: Vec<TitleEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TitleEntry {
    #[serde(rename = "titleId")]
    slug: Option<String>,
    #[serde(default)]
    details: TitleDetails,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct TitleDetails {
    #[serde(rename = "productId")]
    product_id: Option<String>,
    #[serde(rename = "hasEntitlement", default)]
    has_entitlement: bool,
    #[serde(default)]
    programs: Vec<String>,
    #[serde(rename = "userSubscriptions", default)]
    user_subscriptions: Vec<String>,
}

fn extract_games(value: &serde_json::Value) -> Vec<Game> {
    let Ok(response) = serde_json::from_value::<TitlesResponse>(value.clone()) else {
        return Vec::new();
    };

    let mut games: Vec<Game> = response
        .results
        .into_iter()
        .filter_map(|entry| {
            let slug = entry.slug?;
            let is_playable = entry.details.has_entitlement
                || entry
                    .details
                    .programs
                    .iter()
                    .any(|program| entry.details.user_subscriptions.contains(program));
            if !is_playable {
                return None;
            }
            Some(Game {
                id: slug.clone(),
                launch_id: slug,
                metadata_id: entry.details.product_id,
                details: None,
                icon: None,
                box_art: None,
                background: None,
            })
        })
        .collect();

    games.sort_by(|left, right| left.id.cmp(&right.id));
    games.dedup_by(|left, right| left.id == right.id);
    games
}
