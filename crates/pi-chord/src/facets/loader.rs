//! Port of packages/chord/src/facets/loader.ts

#![allow(dead_code, unused_variables)]

use crate::services::errors::clone_error;
use crate::types::LoadedFacets;

pub async fn dispose_loaded_facets(loaded: Vec<LoadedFacets>) -> Vec<pi_js::Error> {
    let futures: Vec<_> = loaded.iter().map(|entry| entry.dispose()).collect();
    let results = futures::future::join_all(futures).await;
    results
        .into_iter()
        .filter_map(|result| match result {
            Ok(()) => None,
            Err(error) => Some(clone_error(&error)),
        })
        .collect()
}
