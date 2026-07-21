use anyhow::{Context, Result};

const CATALOG_CACHE_DIR: &str = "ux0:data/green-vita/cache/catalog-v1";

pub(super) fn provider_path(namespace: &str, filename: &str) -> String {
    format!(
        "{CATALOG_CACHE_DIR}/{}/{}",
        safe_component(namespace),
        filename
    )
}

pub(super) fn game_path(namespace: &str, locale: &str, game_id: &str, filename: &str) -> String {
    format!(
        "{CATALOG_CACHE_DIR}/{}/{}/{}/{}",
        safe_component(namespace),
        safe_component(locale),
        safe_component(game_id),
        filename
    )
}

pub(super) fn read(path: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!("Catalog cache: failed to read {path}: {error}");
            None
        }
    }
}

pub(super) fn write(path: &str, bytes: impl AsRef<[u8]>) -> Result<()> {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .context("catalog cache path has no parent")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create cache directory {directory}"))?;
    crate::fs_utils::write_file_truncating(path, bytes)
}

pub(super) fn clear() -> Result<()> {
    match std::fs::remove_dir_all(CATALOG_CACHE_DIR) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove cache directory {CATALOG_CACHE_DIR}")),
    }
}

fn safe_component(value: &str) -> String {
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
