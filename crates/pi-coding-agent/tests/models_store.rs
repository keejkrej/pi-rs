//! Port of packages/coding-agent/test/models-store.test.ts

use std::sync::Arc;
use std::time::Duration;

use pi_ai::models_store::{ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use pi_ai::types::{AnyModel, Model, ModelCost, ModelInput};
use pi_coding_agent::core::models_store::FileModelsStore;
use pi_coding_agent::vendor::proper_lockfile::{LockOptions, lock};
use pi_js::abort::AbortController;
use pi_js::fs::{read_to_string, write_file as js_write_file};
use pi_js::json::stringify;
use serde_json::{Value, json};
use tempfile::TempDir;

fn test_model(provider: &str, id: &str) -> AnyModel {
    AnyModel::Chat(Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "openai-completions".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    })
}

fn store_entry(provider: &str, ids: &[&str], checked_at: Option<i64>) -> ModelsStoreEntry {
    ModelsStoreEntry {
        models: ids.iter().map(|id| test_model(provider, id)).collect(),
        last_modified: None,
        checked_at,
        etag: None,
    }
}

#[tokio::test]
async fn persists_provider_catalogs_without_replacing_unrelated_providers() {
    let dir = TempDir::new().unwrap();
    let models_path = dir.path().join("models-store.json");
    let models_path_str = models_path.to_str().unwrap();

    let store = FileModelsStore::new(Some(models_path_str));
    store
        .write("one", store_entry("one", &["m1"], Some(100)), None)
        .await
        .unwrap();
    store
        .write("two", store_entry("two", &["m2"], Some(200)), None)
        .await
        .unwrap();

    let reloaded = FileModelsStore::new(Some(models_path_str));
    let one = reloaded.read("one", None).await.unwrap().unwrap();
    let one_ids: Vec<&str> = one.models.iter().map(AnyModel::id).collect();
    assert_eq!(one_ids, vec!["m1"]);
    assert_eq!(one.checked_at, Some(100));

    let two = reloaded.read("two", None).await.unwrap().unwrap();
    let two_ids: Vec<&str> = two.models.iter().map(AnyModel::id).collect();
    assert_eq!(two_ids, vec!["m2"]);

    reloaded.delete("one", None).await.unwrap();
    assert!(reloaded.read("one", None).await.unwrap().is_none());
    let two_after = reloaded.read("two", None).await.unwrap().unwrap();
    let two_ids_after: Vec<&str> = two_after.models.iter().map(AnyModel::id).collect();
    assert_eq!(two_ids_after, vec!["m2"]);
}

#[cfg(unix)]
#[tokio::test]
async fn preserves_the_mode_of_an_existing_models_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let managed_path = dir.path().join("managed-mode.json");
    let managed_path_str = managed_path.to_str().unwrap();

    js_write_file(managed_path_str, "{}", None).unwrap();
    std::fs::set_permissions(&managed_path, std::fs::Permissions::from_mode(0o660)).unwrap();

    let store = FileModelsStore::new(Some(managed_path_str));
    store
        .write("one", store_entry("one", &["m1"], Some(100)), None)
        .await
        .unwrap();

    let mode = std::fs::metadata(&managed_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
}

#[tokio::test]
async fn coalesces_file_reloads_across_concurrent_readers_and_interleaved_instances() {
    let dir = TempDir::new().unwrap();
    let shared_path = dir.path().join("models-store.json");
    let shared_path_str = shared_path.to_str().unwrap();

    let initial = json!({
        "one": { "models": [test_model("one", "old")] },
        "two": { "models": [test_model("two", "m2")] },
    });
    js_write_file(shared_path_str, stringify(&initial), None).unwrap();

    let first = FileModelsStore::new(Some(shared_path_str));
    let second = FileModelsStore::new(Some(shared_path_str));

    let c1 = AbortController::new();
    let c2 = AbortController::new();
    let c3 = AbortController::new();

    let (one, two, missing) = tokio::try_join!(
        first.read(
            "one",
            Some(ModelsStoreOperationOptions {
                signal: Some(c1.signal()),
            }),
        ),
        second.read(
            "two",
            Some(ModelsStoreOperationOptions {
                signal: Some(c2.signal()),
            }),
        ),
        first.read(
            "missing",
            Some(ModelsStoreOperationOptions {
                signal: Some(c3.signal()),
            }),
        ),
    )
    .unwrap();

    assert_eq!(
        one.unwrap().models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["old"]
    );
    assert_eq!(
        two.unwrap().models.iter().map(AnyModel::id).collect::<Vec<_>>(),
        vec!["m2"]
    );
    assert!(missing.is_none());

    let cached = second.read("one", None).await.unwrap().unwrap();
    assert_eq!(cached.models[0].id(), "old");

    let other_path = dir.path().join("other-models-store.json");
    let other_path_str = other_path.to_str().unwrap();
    js_write_file(other_path_str, "{}", None).unwrap();
    let other = FileModelsStore::new(Some(other_path_str));
    assert!(other.read("one", None).await.unwrap().is_none());
    assert!(other.read("one", None).await.unwrap().is_none());

    let third = FileModelsStore::new(Some(shared_path_str));
    let updated = json!({
        "one": { "models": [test_model("one", "newest-model")] }
    });
    js_write_file(shared_path_str, stringify(&updated), None).unwrap();

    let (first_reload, third_reload) = tokio::try_join!(first.read("one", None), third.read("one", None)).unwrap();
    assert_eq!(first_reload.unwrap().models[0].id(), "newest-model");
    assert_eq!(third_reload.unwrap().models[0].id(), "newest-model");
}

#[tokio::test]
async fn aborts_a_waiting_reader_without_breaking_concurrent_readers() {
    let dir = TempDir::new().unwrap();
    let shared_path = dir.path().join("models-store.json");
    let shared_path_str = shared_path.to_str().unwrap();

    let payload = json!({
        "one": { "models": [test_model("one", "stored")] }
    });
    js_write_file(shared_path_str, stringify(&payload), None).unwrap();

    let release = lock(
        shared_path_str,
        LockOptions {
            realpath: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let store = Arc::new(FileModelsStore::new(Some(shared_path_str)));
    let first_ctrl = AbortController::new();
    let second_ctrl = AbortController::new();

    let store_a = store.clone();
    let sig_a = first_ctrl.signal();
    let first_fut = tokio::spawn(async move {
        store_a
            .read("one", Some(ModelsStoreOperationOptions { signal: Some(sig_a) }))
            .await
    });

    let store_b = store.clone();
    let sig_b = second_ctrl.signal();
    let second_fut = tokio::spawn(async move {
        store_b
            .read("one", Some(ModelsStoreOperationOptions { signal: Some(sig_b) }))
            .await
    });

    tokio::time::sleep(Duration::from_millis(15)).await;
    first_ctrl.abort(None);

    let first_err = first_fut.await.unwrap().unwrap_err();
    assert!(first_err.is_abort() || first_err.to_string().to_lowercase().contains("abort"));

    release.release().await.unwrap();
    let second_res = second_fut.await.unwrap().unwrap().unwrap();
    assert_eq!(second_res.models[0].id(), "stored");
}

#[tokio::test]
async fn cancels_a_catalog_write_waiting_for_a_held_file_lock_without_writing_later() {
    let dir = TempDir::new().unwrap();
    let shared_path = dir.path().join("models-store.json");
    let shared_path_str = shared_path.to_str().unwrap();

    let initial = json!({
        "one": { "models": [test_model("one", "existing")] }
    });
    js_write_file(shared_path_str, stringify(&initial), None).unwrap();

    let store = Arc::new(FileModelsStore::new(Some(shared_path_str)));
    let release = lock(
        shared_path_str,
        LockOptions {
            realpath: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let controller = AbortController::new();
    let sig = controller.signal();
    let store_w = store.clone();
    let write_task = tokio::spawn(async move {
        store_w
            .write(
                "two",
                store_entry("two", &["cancelled"], None),
                Some(ModelsStoreOperationOptions { signal: Some(sig) }),
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(15)).await;
    controller.abort(None);

    let err = write_task.await.unwrap().unwrap_err();
    assert!(err.is_abort() || err.to_string().to_lowercase().contains("abort"));

    release.release().await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let stored: Value = serde_json::from_str(&read_to_string(shared_path_str).unwrap()).unwrap();
    assert!(stored.get("one").is_some());
    assert!(stored.get("two").is_none());
}
