//! Provider-neutral API exposed to the application.
//!
//! Concrete services live in sibling modules such as `api_xbox` and, in the
//! future, `api_geforce_now`. UI code should consume the models and backends
//! exposed here rather than provider response formats.

pub mod catalog;
pub(crate) mod streaming;
