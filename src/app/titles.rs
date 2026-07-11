use super::image::{TitleImage, mask_to_circle};
use super::{App, AppState};
use crate::xbox_api::catalog::TitleCatalogDetails;
use crate::xbox_api::catalog_worker::{CatalogResult, ImageKind, clear_catalog_cache};
use std::sync::Arc;

const ICON_CACHE_RADIUS: usize = 16;
const RESULTS_PER_TICK: usize = 2;

#[derive(Clone)]
pub(crate) struct CloudTitle {
    pub(crate) title_id: String,
    pub(crate) product_id: Option<String>,
    pub(crate) details: Option<TitleCatalogDetails>,
    pub(crate) icon: Option<Arc<TitleImage>>,
    pub(crate) box_art: Option<Arc<TitleImage>>,
    pub(crate) background: Option<Arc<TitleImage>>,
}

impl CloudTitle {
    pub(crate) fn display_name(&self) -> &str {
        self.details
            .as_ref()
            .and_then(|details| details.name.as_deref())
            .unwrap_or(&self.title_id)
    }
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

pub(super) fn extract_titles(value: &serde_json::Value) -> Vec<CloudTitle> {
    let Ok(response) = serde_json::from_value::<TitlesResponse>(value.clone()) else {
        return Vec::new();
    };

    let mut titles: Vec<CloudTitle> = response
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
            Some(CloudTitle {
                title_id: slug,
                product_id: entry.details.product_id,
                details: None,
                icon: None,
                box_art: None,
                background: None,
            })
        })
        .collect();

    titles.sort_by(|left, right| left.title_id.cmp(&right.title_id));
    titles.dedup_by(|left, right| left.title_id == right.title_id);
    titles
}

impl App {
    /// The pending-request set matching `kind`, so a fetch isn't re-queued while in flight.
    fn image_pending_set(&mut self, kind: ImageKind) -> &mut std::collections::HashSet<String> {
        match kind {
            ImageKind::Cover => &mut self.service.box_art_pending,
            ImageKind::Background => &mut self.service.background_pending,
            ImageKind::Icon => &mut self.service.icon_pending,
            ImageKind::Avatar => unreachable!("avatar requests aren't tracked per title"),
        }
    }

    fn request_title_image(
        &mut self,
        title_id: String,
        kind: ImageKind,
        url: String,
        prefetch: bool,
    ) {
        let cache_locale = self.settings.locale.as_str().to_lowercase();
        let queued = if prefetch {
            self.service.catalog_worker.prefetch_image(
                title_id.clone(),
                kind,
                url,
                Some(cache_locale),
            )
        } else {
            self.service.catalog_worker.request_image(
                title_id.clone(),
                kind,
                url,
                Some(cache_locale),
            )
        };
        if queued {
            self.image_pending_set(kind).insert(title_id);
        }
    }

    /// Fetches metadata + cover art/background for the highlighted title via `CatalogWorker`.
    fn ensure_title_details_job(&mut self) {
        let AppState::TitleList { selected } = &self.state else {
            return;
        };
        let selected = *selected;
        let Some(title) = self.service.titles.get(selected) else {
            return;
        };
        let title_id = title.title_id.clone();

        if title.details.is_none() {
            self.request_metadata_if_needed(&title_id, title.product_id.clone(), false);
            return;
        }

        let title = &self.service.titles[selected];
        let details = title.details.as_ref().expect("title details checked above");
        let box_art_url = (title.box_art.is_none()
            && !self.service.box_art_pending.contains(&title_id))
        .then(|| details.box_art_url.clone())
        .flatten();
        let background_url = (title.background.is_none()
            && !self.service.background_pending.contains(&title_id))
        .then(|| details.background_url.clone())
        .flatten();

        if let Some(url) = box_art_url {
            self.request_title_image(title_id.clone(), ImageKind::Cover, url, false);
        }
        if let Some(url) = background_url {
            self.request_title_image(title_id, ImageKind::Background, url, false);
        }
    }

    fn ensure_icon_prefetch_job(&mut self) {
        let AppState::TitleList { selected } = &self.state else {
            return;
        };
        let selected = *selected;
        let title_count = self.service.titles.len();
        let missing_metadata = self
            .service
            .titles
            .iter()
            .enumerate()
            .filter(|(index, _)| title_distance(*index, selected, title_count) <= ICON_CACHE_RADIUS)
            .filter(|(_, title)| title.details.is_none())
            .min_by_key(|(index, _)| title_distance(*index, selected, title_count))
            .map(|(_, title)| (title.title_id.clone(), title.product_id.clone()));
        if let Some((title_id, product_id)) = missing_metadata {
            self.request_metadata_if_needed(&title_id, product_id, true);
        }

        let icon_pending = &self.service.icon_pending;
        let next_icon = self
            .service
            .titles
            .iter()
            .enumerate()
            .filter(|(index, _)| title_distance(*index, selected, title_count) <= ICON_CACHE_RADIUS)
            .filter_map(|(index, title)| {
                if title.icon.is_none() && !icon_pending.contains(&title.title_id) {
                    title.details.as_ref()?.icon_url.clone().map(|url| {
                        (
                            title_distance(index, selected, title_count),
                            title.title_id.clone(),
                            url,
                        )
                    })
                } else {
                    None
                }
            })
            .min_by_key(|(distance, _, _)| *distance);
        if let Some((_, title_id, url)) = next_icon {
            self.request_title_image(title_id, ImageKind::Icon, url, true);
        }
    }

    fn prune_title_images(&mut self) {
        let AppState::TitleList { selected } = &self.state else {
            return;
        };
        let selected = *selected;
        let title_count = self.service.titles.len();
        for (index, title) in self.service.titles.iter_mut().enumerate() {
            if index != selected {
                title.box_art = None;
                title.background = None;
            }
            if title_distance(index, selected, title_count) > ICON_CACHE_RADIUS {
                title.icon = None;
            }
        }
    }

    fn request_metadata_if_needed(
        &mut self,
        title_id: &str,
        product_id: Option<String>,
        prefetch: bool,
    ) {
        if self.service.title_detail_pending.contains(title_id) {
            return;
        }
        let Some(product_id) = product_id else {
            if let Some(title) = self
                .service
                .titles
                .iter_mut()
                .find(|title| title.title_id == title_id)
            {
                title.details = Some(TitleCatalogDetails::default());
            }
            return;
        };

        let title_id = title_id.to_owned();
        let queued = if prefetch {
            self.service.catalog_worker.prefetch_metadata(
                title_id.clone(),
                product_id,
                self.settings.locale.market().to_owned(),
                self.settings.locale.as_str().to_lowercase(),
            )
        } else {
            self.service.catalog_worker.request_metadata(
                title_id.clone(),
                product_id,
                self.settings.locale.market().to_owned(),
                self.settings.locale.as_str().to_lowercase(),
            )
        };
        if queued {
            self.service.title_detail_pending.insert(title_id);
        }
    }

    pub(super) async fn pump_title_details(&mut self) -> anyhow::Result<()> {
        self.prune_title_images();
        self.ensure_title_details_job();
        self.ensure_icon_prefetch_job();

        for _ in 0..RESULTS_PER_TICK {
            let Some(result) = self.service.catalog_worker.try_recv() else {
                break;
            };
            match result {
                CatalogResult::Metadata { title_id, details } => {
                    self.service.title_detail_pending.remove(&title_id);
                    if let Some(title) = self
                        .service
                        .titles
                        .iter_mut()
                        .find(|title| title.title_id == title_id)
                    {
                        title.details = Some(details.unwrap_or_default());
                    }
                }
                CatalogResult::Image {
                    title_id,
                    kind,
                    art,
                } => {
                    let image = art.map(|(rgba, width, height)| {
                        let mut image = TitleImage::new(rgba, width, height);
                        if kind == ImageKind::Avatar {
                            mask_to_circle(&mut image);
                        }
                        Arc::new(image)
                    });
                    if kind == ImageKind::Avatar {
                        self.service.avatar = image;
                        continue;
                    }
                    self.image_pending_set(kind).remove(&title_id);
                    let selected = match &self.state {
                        AppState::TitleList { selected } => *selected,
                        _ => continue,
                    };
                    let title_count = self.service.titles.len();
                    if let Some((index, title)) = self
                        .service
                        .titles
                        .iter_mut()
                        .enumerate()
                        .find(|(_, title)| title.title_id == title_id)
                    {
                        match kind {
                            ImageKind::Cover if index == selected => title.box_art = image,
                            ImageKind::Background if index == selected => title.background = image,
                            ImageKind::Icon
                                if title_distance(index, selected, title_count)
                                    <= ICON_CACHE_RADIUS =>
                            {
                                title.icon = image
                            }
                            ImageKind::Cover | ImageKind::Background | ImageKind::Icon => {}
                            ImageKind::Avatar => unreachable!(),
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn highlighted_title(&self) -> Option<&CloudTitle> {
        let AppState::TitleList { selected } = &self.state else {
            return None;
        };
        self.service.titles.get(*selected)
    }

    pub(super) fn invalidate_catalog_for_locale_change(&mut self) {
        if let Err(error) = clear_catalog_cache() {
            eprintln!("Title cache: failed to clear after locale change: {error:#}");
        }
        for title in &mut self.service.titles {
            title.details = None;
            title.icon = None;
            title.box_art = None;
            title.background = None;
        }
        self.service.restart_catalog_worker();
    }
}

fn title_distance(index: usize, selected: usize, count: usize) -> usize {
    let direct = index.abs_diff(selected);
    direct.min(count.saturating_sub(direct))
}
