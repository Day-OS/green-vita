//! Provider-neutral metadata/image worker and on-disk game catalog cache.

use super::{GameCatalogBackend, GameDetails, cache};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased};
use reqwest::Client;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

const MAX_PENDING_CATALOG_JOBS: usize = 4;
const MAX_PENDING_PREFETCH_JOBS: usize = 4;
const MAX_PENDING_CATALOG_RESULTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Cover,
    Background,
    Icon,
    Avatar,
}

enum CatalogJob {
    Metadata {
        game_id: String,
        metadata_id: String,
        market: String,
        language: String,
    },
    Image {
        game_id: String,
        kind: ImageKind,
        url: String,
        cache_locale: Option<String>,
    },
}

impl CatalogJob {
    async fn run(self, client: &Client, backend: &GameCatalogBackend) -> CatalogResult {
        match self {
            Self::Metadata {
                game_id,
                metadata_id,
                market,
                language,
            } => {
                if let Some(details) =
                    load_cached_metadata(backend.cache_namespace(), &language, &game_id)
                {
                    return CatalogResult::Metadata {
                        game_id,
                        details: Some(details),
                    };
                }
                let result = backend
                    .fetch_details(client, &metadata_id, &market, &language)
                    .await;
                if let Err(error) = &result {
                    eprintln!("Catalog worker: metadata for {game_id} failed: {error:#}");
                } else if let Ok(details) = &result
                    && let Err(error) = save_cached_metadata(
                        backend.cache_namespace(),
                        &language,
                        &game_id,
                        details,
                    )
                {
                    eprintln!("Catalog worker: failed to cache metadata for {game_id}: {error:#}");
                }
                CatalogResult::Metadata {
                    game_id,
                    details: result.ok(),
                }
            }
            Self::Image {
                game_id,
                kind,
                url,
                cache_locale,
            } => {
                let result = load_or_fetch_image(
                    client,
                    backend.cache_namespace(),
                    &game_id,
                    kind,
                    &url,
                    cache_locale.as_deref(),
                )
                .await;
                if let Err(error) = &result {
                    eprintln!("Catalog worker: {kind:?} image for {game_id} failed: {error:#}");
                }
                CatalogResult::Image {
                    game_id,
                    kind,
                    art: result.ok(),
                }
            }
        }
    }
}

pub enum CatalogResult {
    Metadata {
        game_id: String,
        details: Option<GameDetails>,
    },
    Image {
        game_id: String,
        kind: ImageKind,
        art: Option<(Vec<u8>, u32, u32)>,
    },
}

pub struct CatalogWorker {
    jobs: Sender<CatalogJob>,
    prefetch_jobs: Sender<CatalogJob>,
    results: Receiver<CatalogResult>,
}

impl CatalogWorker {
    pub fn spawn(backend: GameCatalogBackend) -> Self {
        let (job_tx, job_rx) = bounded::<CatalogJob>(MAX_PENDING_CATALOG_JOBS);
        let (prefetch_tx, prefetch_rx) = bounded::<CatalogJob>(MAX_PENDING_PREFETCH_JOBS);
        let (result_tx, result_rx) = bounded::<CatalogResult>(MAX_PENDING_CATALOG_RESULTS);

        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                eprintln!("Catalog worker: failed to build tokio runtime, thread exiting");
                return;
            };
            let client = Client::builder()
                .timeout(Duration::from_secs(8))
                .build()
                .unwrap_or_default();

            loop {
                let job = select_biased! {
                    recv(job_rx) -> job => job,
                    recv(prefetch_rx) -> job => job,
                };
                let Ok(job) = job else {
                    break;
                };
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    runtime.block_on(job.run(&client, &backend))
                }));
                let result = match outcome {
                    Ok(result) => result,
                    Err(_) => {
                        eprintln!("Catalog worker: job panicked; continuing");
                        continue;
                    }
                };
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        });

        Self {
            jobs: job_tx,
            prefetch_jobs: prefetch_tx,
            results: result_rx,
        }
    }

    pub fn request_metadata(
        &self,
        game_id: String,
        metadata_id: String,
        market: String,
        language: String,
    ) -> bool {
        self.send_job(CatalogJob::Metadata {
            game_id,
            metadata_id,
            market,
            language,
        })
    }

    pub fn prefetch_metadata(
        &self,
        game_id: String,
        metadata_id: String,
        market: String,
        language: String,
    ) -> bool {
        self.send_prefetch(CatalogJob::Metadata {
            game_id,
            metadata_id,
            market,
            language,
        })
    }

    /// `cache_locale` is `None` for non-catalog images such as the profile avatar.
    pub fn request_image(
        &self,
        game_id: String,
        kind: ImageKind,
        url: String,
        cache_locale: Option<String>,
    ) -> bool {
        self.send_job(CatalogJob::Image {
            game_id,
            kind,
            url,
            cache_locale,
        })
    }

    pub fn prefetch_image(
        &self,
        game_id: String,
        kind: ImageKind,
        url: String,
        cache_locale: Option<String>,
    ) -> bool {
        self.send_prefetch(CatalogJob::Image {
            game_id,
            kind,
            url,
            cache_locale,
        })
    }

    pub fn try_recv(&self) -> Option<CatalogResult> {
        self.results.try_recv().ok()
    }

    fn send_job(&self, job: CatalogJob) -> bool {
        match self.jobs.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn send_prefetch(&self, job: CatalogJob) -> bool {
        match self.prefetch_jobs.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

impl ImageKind {
    fn dimensions(self) -> (u32, u32, bool) {
        match self {
            Self::Cover => (256, 256, false),
            Self::Background => (512, 512, false),
            Self::Icon | Self::Avatar => (64, 64, true),
        }
    }

    fn cache_filename(self) -> Option<&'static str> {
        match self {
            Self::Cover => Some("cover.image"),
            Self::Background => Some("background.image"),
            Self::Icon => Some("icon.image"),
            Self::Avatar => None,
        }
    }
}

async fn load_or_fetch_image(
    client: &Client,
    namespace: &str,
    game_id: &str,
    kind: ImageKind,
    url: &str,
    cache_locale: Option<&str>,
) -> Result<(Vec<u8>, u32, u32)> {
    let cache_path = cache_locale
        .zip(kind.cache_filename())
        .map(|(locale, filename)| cache::game_path(namespace, locale, game_id, filename));

    if let Some(path) = cache_path.as_deref()
        && let Some(bytes) = cache::read(path)
    {
        let (max_width, max_height, pad_to_bounds) = kind.dimensions();
        match decode_image_rgba(&bytes, max_width, max_height, pad_to_bounds) {
            Ok(art) => return Ok(art),
            Err(error) => {
                eprintln!("Catalog worker: invalid cached {kind:?} for {game_id}: {error:#}");
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let bytes = client
        .get(url)
        .send()
        .await
        .context("catalog image request failed")?
        .error_for_status()
        .context("catalog image request returned an error status")?
        .bytes()
        .await
        .context("failed to read catalog image response body")?;
    let (max_width, max_height, pad_to_bounds) = kind.dimensions();
    let art = decode_image_rgba(&bytes, max_width, max_height, pad_to_bounds)?;
    if let Some(path) = cache_path.as_deref()
        && let Err(error) = cache::write(path, &bytes)
    {
        eprintln!("Catalog worker: failed to cache {kind:?} for {game_id}: {error:#}");
    }
    Ok(art)
}

fn decode_image_rgba(
    bytes: &[u8],
    max_width: u32,
    max_height: u32,
    pad_to_bounds: bool,
) -> Result<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory(bytes).context("failed to decode catalog image")?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let scale = (max_width as f32 / width as f32)
        .min(max_height as f32 / height as f32)
        .min(1.0);
    let resized_width = ((width as f32 * scale).round() as u32).max(1);
    let resized_height = ((height as f32 * scale).round() as u32).max(1);
    let resized = if (resized_width, resized_height) == (width, height) {
        rgba
    } else {
        image::imageops::resize(
            &rgba,
            resized_width,
            resized_height,
            image::imageops::FilterType::Triangle,
        )
    };

    if !pad_to_bounds {
        return Ok((resized.into_raw(), resized_width, resized_height));
    }

    let mut canvas = vec![0; (max_width * max_height * 4) as usize];
    let offset_x = (max_width - resized_width) / 2;
    let offset_y = (max_height - resized_height) / 2;
    let pixels = resized.as_raw();
    for row in 0..resized_height {
        let source = (row * resized_width * 4) as usize;
        let destination = (((row + offset_y) * max_width + offset_x) * 4) as usize;
        let length = (resized_width * 4) as usize;
        canvas[destination..destination + length].copy_from_slice(&pixels[source..source + length]);
    }
    Ok((canvas, max_width, max_height))
}

fn load_cached_metadata(namespace: &str, locale: &str, game_id: &str) -> Option<GameDetails> {
    let path = cache::game_path(namespace, locale, game_id, "metadata.json");
    let bytes = cache::read(&path)?;
    match serde_json::from_slice(&bytes) {
        Ok(details) => Some(details),
        Err(error) => {
            eprintln!("Catalog worker: invalid cached metadata for {game_id}: {error}");
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn save_cached_metadata(
    namespace: &str,
    locale: &str,
    game_id: &str,
    details: &GameDetails,
) -> Result<()> {
    let path = cache::game_path(namespace, locale, game_id, "metadata.json");
    let bytes = serde_json::to_vec(details).context("failed to serialize catalog metadata")?;
    cache::write(&path, bytes)
}

pub fn clear_catalog_cache() -> Result<()> {
    cache::clear()
}
