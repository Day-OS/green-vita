use super::auth::{GFN_CLIENT_VERSION, GFN_LCARS_CLIENT_ID, GFN_USER_AGENT, NvidiaAuth};
use crate::api::catalog::{Game, GameDetails};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

const CLOUDMATCH_BASE: &str = "https://prod.cloudmatchbeta.nvidiagrid.net/";
const GRAPHQL_URL: &str = "https://games.geforce.com/graphql";
const STEAM_SYNC_URL: &str = "https://als.geforcenow.com/v1/sync/STEAM";

#[derive(Deserialize)]
struct ServerInfoResponse {
    #[serde(rename = "requestStatus")]
    request_status: Option<ServerRequestStatus>,
}

#[derive(Deserialize)]
struct ServerRequestStatus {
    #[serde(rename = "serverId")]
    server_id: Option<String>,
}

pub async fn load_games(auth: &NvidiaAuth) -> Result<Vec<Game>> {
    let token = auth.token()?;
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap_or_default();
    let server: ServerInfoResponse = client
        .get(format!("{CLOUDMATCH_BASE}v2/serverInfo"))
        .headers(gfn_headers(token, false)?)
        .send()
        .await?
        .json()
        .await
        .unwrap_or(ServerInfoResponse {
            request_status: None,
        });
    let vpc_id = server
        .request_status
        .and_then(|status| status.server_id)
        .unwrap_or_else(|| "GFN-PC".to_owned());

    let mut games = fetch_library(&client, token, &vpc_id).await?;
    if games.is_empty() && request_steam_sync(&client, token).await {
        // Linking a Steam account does not itself populate the GFN library. Give
        // NVIDIA's asynchronous sync job the same grace period used by OpenNOW.
        tokio::time::sleep(Duration::from_secs(10)).await;
        games = fetch_library(&client, token, &vpc_id).await?;
    }
    Ok(games)
}

async fn fetch_library(client: &Client, token: &str, vpc_id: &str) -> Result<Vec<Game>> {
    let query = r#"query GetLibraryApps(
      $vpcId: String!, $locale: String!, $sortString: String!,
      $fetchCount: Int!, $cursor: String!, $filters: AppFilterFields!
    ) {
      apps(vpcId: $vpcId, language: $locale, orderBy: $sortString,
        first: $fetchCount, after: $cursor, filters: $filters) {
        pageInfo { hasNextPage endCursor }
        items {
          id title
          images { KEY_ART KEY_IMAGE GAME_BOX_ART TV_BANNER HERO_IMAGE MARQUEE_HERO_IMAGE FEATURE_IMAGE }
          variants { id appStore gfn { library { status selected } } }
        }
      }
    }"#;

    let mut cursor = String::new();
    let mut games = Vec::new();
    let mut ids = HashSet::new();
    for _ in 0..25 {
        let response = client
            .post(GRAPHQL_URL)
            .headers(gfn_headers(token, true)?)
            .json(&json!({
                "query": query,
                "variables": {
                    "vpcId": vpc_id,
                    "locale": "en_US",
                    "sortString": "variants.gfn.library.lastPlayedDate:DESC,computedValues.libraryAddedDate:DESC,sortName:ASC",
                    "fetchCount": 200,
                    "cursor": cursor,
                    "filters": {
                        "variants": { "gfn": { "library": { "status": { "notEquals": "NOT_OWNED" } } } }
                    }
                }
            }))
            .send()
            .await
            .context("GeForce NOW library request failed")?;
        if !response.status().is_success() {
            bail!(
                "GeForce NOW library request failed ({}): {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
        let payload: Value = response.json().await?;
        if let Some(errors) = payload.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            bail!("GeForce NOW library returned GraphQL errors: {errors:?}");
        }
        let apps = payload
            .pointer("/data/apps/items")
            .and_then(Value::as_array)
            .context("GeForce NOW library response had no apps")?;
        for app in apps {
            if let Some(game) = map_game(app)
                && ids.insert(game.id.clone())
            {
                games.push(game);
            }
        }
        let page = payload
            .pointer("/data/apps/pageInfo")
            .unwrap_or(&Value::Null);
        if !page
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }
        cursor = page
            .get("endCursor")
            .and_then(Value::as_str)
            .context("GeForce NOW pagination omitted its cursor")?
            .to_owned();
    }
    games.sort_by(|left, right| left.display_name().cmp(right.display_name()));
    Ok(games)
}

async fn request_steam_sync(client: &Client, token: &str) -> bool {
    match client
        .post(STEAM_SYNC_URL)
        .headers(steam_sync_headers(token))
        .json(&json!({}))
        .send()
        .await
    {
        Ok(response) if response.status() == reqwest::StatusCode::ACCEPTED => true,
        Ok(response) => {
            eprintln!(
                "GeForce NOW Steam sync was not accepted ({})",
                response.status()
            );
            false
        }
        Err(error) => {
            eprintln!("GeForce NOW Steam sync request failed: {error:#}");
            false
        }
    }
}

fn map_game(app: &Value) -> Option<Game> {
    let id = app.get("id")?.as_str()?.to_owned();
    let title = app.get("title")?.as_str()?.to_owned();
    let variants = app.get("variants")?.as_array()?;
    let selected = variants
        .iter()
        .find(|variant| {
            variant
                .pointer("/gfn/library/selected")
                .and_then(Value::as_bool)
                == Some(true)
        })
        .or_else(|| variants.first())?;
    let launch_id = selected.get("id")?.as_str()?.to_owned();
    let images = app.get("images");
    let box_art_url = first_image(images, &["GAME_BOX_ART", "KEY_ART", "KEY_IMAGE"]);
    let background_url = first_image(
        images,
        &[
            "MARQUEE_HERO_IMAGE",
            "HERO_IMAGE",
            "TV_BANNER",
            "FEATURE_IMAGE",
        ],
    );
    let genres = app
        .get("genres")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(Game {
        id,
        launch_id,
        metadata_id: None,
        details: Some(GameDetails {
            name: Some(title),
            description: None,
            box_art_url: box_art_url.clone(),
            background_url,
            icon_url: box_art_url,
            genres,
            developer: None,
            publisher: None,
            ..GameDetails::default()
        }),
        icon: None,
        box_art: None,
        background: None,
    })
}

fn first_image(images: Option<&Value>, keys: &[&str]) -> Option<String> {
    let images = images?;
    for key in keys {
        let Some(value) = images.get(key) else {
            continue;
        };
        if let Some(url) = value.as_str() {
            return Some(url.to_owned());
        }
        if let Some(url) = value
            .as_array()
            .and_then(|items| items.first())
            .and_then(Value::as_str)
        {
            return Some(url.to_owned());
        }
    }
    None
}

fn gfn_headers(token: &str, graphql: bool) -> Result<reqwest::header::HeaderMap> {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    let authorization = format!("GFNJWT {token}");
    for (name, value) in [
        ("accept", "application/json"),
        ("authorization", authorization.as_str()),
        ("nv-client-id", GFN_LCARS_CLIENT_ID),
        ("nv-client-type", "NATIVE"),
        ("nv-client-version", GFN_CLIENT_VERSION),
        ("nv-client-streamer", "NVIDIA-CLASSIC"),
        ("nv-device-os", "LINUX"),
        ("nv-device-type", "DESKTOP"),
        ("nv-device-make", "UNKNOWN"),
        ("nv-device-model", "UNKNOWN"),
        ("nv-browser-type", "CHROME"),
        ("user-agent", GFN_USER_AGENT),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    if graphql {
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert(
            "origin",
            HeaderValue::from_static("https://play.geforcenow.com"),
        );
        headers.insert(
            "referer",
            HeaderValue::from_static("https://play.geforcenow.com/"),
        );
    }
    Ok(headers)
}

fn steam_sync_headers(token: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    let authorization = format!("Bearer {token}");
    if let Ok(value) = HeaderValue::from_str(&authorization) {
        headers.insert("authorization", value);
    }
    headers.insert(
        "accept",
        HeaderValue::from_static("application/json, text/plain, */*"),
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "origin",
        HeaderValue::from_static("https://play.geforcenow.com"),
    );
    headers.insert(
        "referer",
        HeaderValue::from_static("https://play.geforcenow.com/"),
    );
    headers.insert("user-agent", HeaderValue::from_static(GFN_USER_AGENT));
    headers
}
