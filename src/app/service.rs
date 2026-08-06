use super::image::{self, TitleImage};
use crate::api::CloudProvider;
use crate::api::catalog::worker::CatalogWorker;
use crate::api::catalog::{Game, GameCatalogBackend};
use crate::api_geforce_now::auth::NvidiaAuth;
use crate::{ApiClient, ApiClientConfig, Console, MsalAuth};
use std::collections::HashSet;
use std::sync::Arc;

pub(crate) struct Service {
    pub(crate) api: ApiClient,
    pub(super) auth: MsalAuth,
    pub(super) nvidia_auth: NvidiaAuth,
    pub(crate) cloud_provider: CloudProvider,
    pub(crate) titles: Vec<Game>,
    pub(crate) consoles: Vec<Console>,
    pub(crate) catalog_worker: CatalogWorker,
    pub(crate) catalog_backend: GameCatalogBackend,
    pub(crate) title_detail_pending: HashSet<String>,
    pub(crate) box_art_pending: HashSet<String>,
    pub(crate) background_pending: HashSet<String>,
    pub(crate) icon_pending: HashSet<String>,
    pub(crate) avatar: Option<Arc<TitleImage>>,
    pub(crate) gamertag: Option<String>,
    pub(crate) gamerscore: Option<String>,
    pub(crate) logo: Arc<TitleImage>,
}

impl Service {
    pub(super) fn new(locale: &str) -> Self {
        let mut api = ApiClient::new(ApiClientConfig::default());
        api.config.locale = locale.to_owned();

        let catalog_backend = GameCatalogBackend::xcloud(api.clone());
        let catalog_worker = CatalogWorker::spawn(catalog_backend.clone());
        Self {
            api,
            auth: MsalAuth::new(),
            nvidia_auth: NvidiaAuth::new(),
            cloud_provider: CloudProvider::Xbox,
            titles: Vec::new(),
            consoles: Vec::new(),
            catalog_worker,
            catalog_backend,
            title_detail_pending: HashSet::new(),
            box_art_pending: HashSet::new(),
            background_pending: HashSet::new(),
            icon_pending: HashSet::new(),
            avatar: None,
            gamertag: None,
            gamerscore: None,
            logo: image::load_bundled_logo(),
        }
    }

    pub(super) fn logout(&mut self) {
        match self.cloud_provider {
            CloudProvider::Xbox => self.auth.logout(),
            CloudProvider::GeForceNow => self.nvidia_auth.logout(),
        }
        self.titles.clear();
        self.consoles.clear();
        self.avatar = None;
        self.gamertag = None;
        self.gamerscore = None;
        self.restart_catalog_worker();
    }

    pub(super) fn title_name_or_id(&self, title_id: &str) -> String {
        self.titles
            .iter()
            .find(|title| title.id == title_id)
            .map(|title| title.display_name().to_owned())
            .unwrap_or_else(|| title_id.to_owned())
    }

    pub(super) fn restart_catalog_worker(&mut self) {
        self.catalog_worker = CatalogWorker::spawn(self.catalog_backend.clone());
        self.title_detail_pending.clear();
        self.box_art_pending.clear();
        self.background_pending.clear();
        self.icon_pending.clear();
    }

    pub(super) fn refresh_xcloud_catalog_backend(&mut self) {
        self.cloud_provider = CloudProvider::Xbox;
        self.catalog_backend = GameCatalogBackend::xcloud(self.api.clone());
        self.restart_catalog_worker();
    }

    pub(super) fn use_geforce_now_catalog_backend(&mut self) {
        self.cloud_provider = CloudProvider::GeForceNow;
        self.catalog_backend = GameCatalogBackend::geforce_now(self.nvidia_auth.clone());
        self.restart_catalog_worker();
    }
}
