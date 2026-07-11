//! Runs catalog/box-art fetches on one dedicated OS thread with its own tokio runtime, so a
//! blocking connect/read can't freeze the SDL event loop. Fetched bytes are cached on disk under
//! `ux0:data/xcloud-rust/cache/catalog-v1/{locale}/{title_id}/`.

use crate::xbox_api::catalog;
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased};
use reqwest::Client;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

const MAX_PENDING_CATALOG_JOBS: usize = 4;
const MAX_PENDING_PREFETCH_JOBS: usize = 4;
const MAX_PENDING_CATALOG_RESULTS: usize = 8;
const CATALOG_CACHE_DIR: &str = "ux0:data/xcloud-rust/cache/catalog-v1";

/// Which of a title's images a fetched/decoded result is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Cover,
    Background,
    Icon,
    Avatar,
}

enum CatalogJob {
    Metadata {
        title_id: String,
        product_id: String,
        market: String,
        language: String,
    },
    Image {
        title_id: String,
        kind: ImageKind,
        url: String,
        cache_locale: Option<String>,
    },
}

impl CatalogJob {
    async fn run(self, client: &Client) -> CatalogResult {
        match self {
            CatalogJob::Metadata {
                title_id,
                product_id,
                market,
                language,
            } => {
                if let Some(details) = load_cached_metadata(&language, &title_id) {
                    return CatalogResult::Metadata {
                        title_id,
                        details: Some(details),
                    };
                }
                let result =
                    catalog::fetch_title_details(client, &product_id, &market, &language).await;
                if let Err(error) = &result {
                    eprintln!("Catalog worker: metadata for {title_id} failed: {error:#}");
                } else if let Ok(details) = &result
                    && let Err(error) = save_cached_metadata(&language, &title_id, details)
                {
                    eprintln!("Catalog worker: failed to cache metadata for {title_id}: {error:#}");
                }
                CatalogResult::Metadata {
                    title_id,
                    details: result.ok(),
                }
            }
            CatalogJob::Image {
                title_id,
                kind,
                url,
                cache_locale,
            } => {
                let result =
                    load_or_fetch_image(client, &title_id, kind, &url, cache_locale.as_deref())
                        .await;
                if let Err(error) = &result {
                    eprintln!("Catalog worker: {kind:?} image for {title_id} failed: {error:#}");
                }
                CatalogResult::Image {
                    title_id,
                    kind,
                    art: result.ok(),
                }
            }
        }
    }
}

pub enum CatalogResult {
    Metadata {
        title_id: String,
        details: Option<catalog::TitleCatalogDetails>,
    },
    Image {
        title_id: String,
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
    pub fn spawn() -> Self {
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
                let outcome = catch_unwind(AssertUnwindSafe(|| runtime.block_on(job.run(&client))));
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
        title_id: String,
        product_id: String,
        market: String,
        language: String,
    ) -> bool {
        self.send_job(CatalogJob::Metadata {
            title_id,
            product_id,
            market,
            language,
        })
    }

    pub fn prefetch_metadata(
        &self,
        title_id: String,
        product_id: String,
        market: String,
        language: String,
    ) -> bool {
        self.send_prefetch(CatalogJob::Metadata {
            title_id,
            product_id,
            market,
            language,
        })
    }

    /// `cache_locale` is `None` for the avatar, the only image not cached on disk per-locale.
    pub fn request_image(
        &self,
        title_id: String,
        kind: ImageKind,
        url: String,
        cache_locale: Option<String>,
    ) -> bool {
        self.send_job(CatalogJob::Image {
            title_id,
            kind,
            url,
            cache_locale,
        })
    }

    pub fn prefetch_image(
        &self,
        title_id: String,
        kind: ImageKind,
        url: String,
        cache_locale: Option<String>,
    ) -> bool {
        self.send_prefetch(CatalogJob::Image {
            title_id,
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
    title_id: &str,
    kind: ImageKind,
    url: &str,
    cache_locale: Option<&str>,
) -> Result<(Vec<u8>, u32, u32)> {
    let cache_path = cache_locale
        .zip(kind.cache_filename())
        .map(|(locale, filename)| catalog_cache_path(locale, title_id, filename));

    if let Some(path) = cache_path.as_deref()
        && let Some(bytes) = read_cached_file(path)
    {
        let (max_width, max_height, pad_to_bounds) = kind.dimensions();
        match catalog::decode_image_rgba(&bytes, max_width, max_height, pad_to_bounds) {
            Ok(art) => return Ok(art),
            Err(error) => {
                eprintln!("Catalog worker: invalid cached {kind:?} for {title_id}: {error:#}");
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let bytes = catalog::fetch_image_bytes(client, url).await?;
    let (max_width, max_height, pad_to_bounds) = kind.dimensions();
    let art = catalog::decode_image_rgba(&bytes, max_width, max_height, pad_to_bounds)?;
    if let Some(path) = cache_path.as_deref()
        && let Err(error) = write_cached_file(path, &bytes)
    {
        eprintln!("Catalog worker: failed to cache {kind:?} for {title_id}: {error:#}");
    }
    Ok(art)
}

fn load_cached_metadata(locale: &str, title_id: &str) -> Option<catalog::TitleCatalogDetails> {
    let path = catalog_cache_path(locale, title_id, "metadata.json");
    let bytes = read_cached_file(&path)?;
    match serde_json::from_slice(&bytes) {
        Ok(details) => Some(details),
        Err(error) => {
            eprintln!("Catalog worker: invalid cached metadata for {title_id}: {error}");
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn save_cached_metadata(
    locale: &str,
    title_id: &str,
    details: &catalog::TitleCatalogDetails,
) -> Result<()> {
    let path = catalog_cache_path(locale, title_id, "metadata.json");
    let bytes = serde_json::to_vec(details).context("failed to serialize catalog metadata")?;
    write_cached_file(&path, bytes)
}

fn read_cached_file(path: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!("Catalog worker: failed to read {path}: {error}");
            None
        }
    }
}

fn write_cached_file(path: &str, bytes: impl AsRef<[u8]>) -> Result<()> {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .context("catalog cache path has no parent")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create cache directory {directory}"))?;
    crate::fs_utils::write_file_truncating(path, bytes)
}

fn catalog_cache_path(locale: &str, title_id: &str, filename: &str) -> String {
    format!(
        "{CATALOG_CACHE_DIR}/{}/{}/{}",
        safe_cache_component(locale),
        safe_cache_component(title_id),
        filename
    )
}

fn safe_cache_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "_".to_owned()
    } else {
        sanitized
    }
}

pub fn clear_catalog_cache() -> Result<()> {
    match std::fs::remove_dir_all(CATALOG_CACHE_DIR) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove cache directory {CATALOG_CACHE_DIR}")),
    }
}
