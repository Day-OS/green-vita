//! Fetches title metadata (name, description, cover art) from the Microsoft Store's public catalog - needs no authentication, unlike the xCloud/xHome APIs elsewhere in this crate.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleCatalogDetails {
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

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    #[serde(default, rename = "Products")]
    products: Vec<CatalogProduct>,
}

#[derive(Debug, Deserialize)]
struct CatalogProduct {
    #[serde(default, rename = "LocalizedProperties")]
    localized_properties: Vec<CatalogLocalizedProperties>,
    #[serde(default, rename = "MarketProperties")]
    market_properties: Vec<CatalogMarketProperties>,
}

#[derive(Debug, Deserialize)]
struct CatalogLocalizedProperties {
    #[serde(rename = "ProductTitle")]
    product_title: Option<String>,
    #[serde(rename = "ShortDescription")]
    short_description: Option<String>,
    #[serde(rename = "ProductDescription")]
    product_description: Option<String>,
    #[serde(rename = "DeveloperName")]
    developer_name: Option<String>,
    #[serde(rename = "PublisherName")]
    publisher_name: Option<String>,
    #[serde(default, rename = "Genres")]
    genres: Vec<CatalogGenre>,
    #[serde(default, rename = "Images")]
    images: Vec<CatalogImage>,
}

#[derive(Debug, Deserialize)]
struct CatalogGenre {
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogImage {
    #[serde(rename = "ImagePurpose")]
    purpose: String,
    #[serde(rename = "Uri")]
    uri: String,
}

#[derive(Debug, Deserialize)]
struct CatalogMarketProperties {
    #[serde(rename = "OriginalReleaseDate")]
    original_release_date: Option<String>,
    #[serde(default, rename = "UsageData")]
    usage_data: Vec<CatalogUsageData>,
    #[serde(default, rename = "ContentRatings")]
    content_ratings: Vec<CatalogContentRating>,
}

#[derive(Debug, Deserialize)]
struct CatalogUsageData {
    #[serde(rename = "AverageRating")]
    average_rating: Option<f32>,
    #[serde(rename = "RatingCount")]
    rating_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CatalogContentRating {
    #[serde(rename = "RatingSystem")]
    rating_system: Option<String>,
    #[serde(rename = "RatingId")]
    rating_id: Option<String>,
}

/// Shared shape of the two HTTP fetches in this module.
async fn get_bytes(client: &Client, url: &str, label: &str) -> Result<bytes::Bytes> {
    client
        .get(url)
        .send()
        .await
        .with_context(|| format!("{label} request failed"))?
        .error_for_status()
        .with_context(|| format!("{label} request returned an error status"))?
        .bytes()
        .await
        .with_context(|| format!("failed to read {label} response body"))
}

/// Fetches display metadata for a single product by its Store `productId`, localized to
/// `market`/`language` (e.g. `"BR"`/`"pt-br"` - see `Settings::locale`, `Locale::market`).
pub async fn fetch_title_details(
    client: &Client,
    product_id: &str,
    market: &str,
    language: &str,
) -> Result<TitleCatalogDetails> {
    let url = format!(
        "https://displaycatalog.mp.microsoft.com/v7.0/products?bigIds={product_id}&market={market}&languages={language}&fieldsTemplate=Details"
    );
    let bytes = get_bytes(client, &url, "Catalog").await?;
    let response: CatalogResponse =
        serde_json::from_slice(&bytes).context("failed to parse catalog response")?;

    let Some(product) = response.products.into_iter().next() else {
        bail!("no catalog listing found for product {product_id}");
    };
    let market = product.market_properties.into_iter().next();
    let Some(localized) = product.localized_properties.into_iter().next() else {
        bail!("no localized catalog listing found for product {product_id}");
    };

    let find_image = |purpose: &str| {
        localized
            .images
            .iter()
            .find(|image| image.purpose == purpose)
    };
    // `Poster` is a tall box-art-style crop; `BoxArt` (square) is the fallback. The `?h=`/`?w=`
    // query asks the CDN for a resized thumbnail instead of the multi-megapixel original.
    let cover_image = find_image("Poster").or_else(|| find_image("BoxArt"));
    let box_art_url = cover_image.map(|image| format!("https:{}?h=300", image.uri));
    let icon_url = find_image("Logo")
        .or(cover_image)
        .map(|image| format!("https:{}?h=64", image.uri));
    // `SuperHeroArt` is the wide backdrop banner; `Screenshot` is the fallback.
    let background_url = find_image("SuperHeroArt")
        .or_else(|| find_image("Screenshot"))
        .map(|image| format!("https:{}?w=512", image.uri));

    let usage_data = market.as_ref().and_then(|m| m.usage_data.first());
    // `rating_id` already comes back board-qualified (e.g. "COB-AU:M"), so `rating_system` is
    // only a fallback, never a prefix.
    let content_rating = market
        .as_ref()
        .and_then(|m| m.content_ratings.first())
        .and_then(|rating| {
            rating
                .rating_id
                .clone()
                .or_else(|| rating.rating_system.clone())
        });

    Ok(TitleCatalogDetails {
        name: localized.product_title,
        description: localized
            .short_description
            .or(localized.product_description),
        box_art_url,
        background_url,
        icon_url,
        genres: localized
            .genres
            .iter()
            .filter_map(|genre| genre.name.clone())
            .collect(),
        developer: localized.developer_name,
        publisher: localized.publisher_name,
        release_date: market
            .as_ref()
            .and_then(|m| m.original_release_date.clone()),
        average_rating: usage_data.and_then(|u| u.average_rating),
        rating_count: usage_data.and_then(|u| u.rating_count),
        content_rating,
    })
}
