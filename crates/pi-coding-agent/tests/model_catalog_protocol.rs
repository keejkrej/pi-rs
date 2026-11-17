//! Port of packages/coding-agent/test/model-catalog-protocol.test.ts
//! and scripts/model-catalog-protocol.ts.

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::models::{ModelsRefreshOptions, ModelsSimpleStreamOptions};
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::types::{Context, Message, SimpleStreamOptions, UserMessage, UserMessageContent};
use pi_coding_agent::config::VERSION;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::Error;
use pi_js::fetch::testing::mock_fetch;
use pi_js::json::stringify;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

const MODEL_CATALOG_SCHEMA_VERSION: u64 = 1;
const MODEL_CATALOG_PREFIX: &str = "models/v1";
const MODEL_CATALOG_INDEX_KEY: &str = "models/v1/index.json";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Representation {
    Legacy,
    Typed,
}

#[derive(Clone)]
struct CatalogIndexEntry {
    minimum_pi_version: String,
    revision: String,
}

#[derive(Clone)]
struct CatalogIndex {
    _schema_version: u64,
    default_revision: String,
    catalogs: Vec<CatalogIndexEntry>,
}

enum CatalogRequest {
    Catalog {
        pi_version: Option<String>,
        representation: Representation,
    },
    Redirect {
        location: String,
    },
    Invalid {
        error: String,
    },
}

fn is_valid_revision(s: &str) -> bool {
    let Some(hex) = s.strip_prefix("sha256-") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_valid_model_type(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn parse_pi_version(version: &str) -> Option<(u64, u64, u64, String)> {
    let trimmed = version.trim().strip_prefix('v').unwrap_or(version.trim());
    let main_and_build = trimmed.split('+').next()?;
    let (core, pre) = match main_and_build.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, p.to_owned()),
        Some(_) => return None,
        None => (main_and_build, String::new()),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre))
}

fn compare_pi_versions(left: &(u64, u64, u64, String), right: &(u64, u64, u64, String)) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let core_cmp = (left.0, left.1, left.2).cmp(&(right.0, right.1, right.2));
    if core_cmp != Ordering::Equal {
        return core_cmp;
    }
    if left.3 == right.3 {
        return Ordering::Equal;
    }
    if left.3.is_empty() {
        return Ordering::Greater;
    }
    if right.3.is_empty() {
        return Ordering::Less;
    }
    let l_parts: Vec<&str> = left.3.split('.').collect();
    let r_parts: Vec<&str> = right.3.split('.').collect();
    for idx in 0..l_parts.len().max(r_parts.len()) {
        let Some(lp) = l_parts.get(idx).copied() else {
            return Ordering::Less;
        };
        let Some(rp) = r_parts.get(idx).copied() else {
            return Ordering::Greater;
        };
        if lp == rp {
            continue;
        }
        match (lp.parse::<u64>(), rp.parse::<u64>()) {
            (Ok(ln), Ok(rn)) => return ln.cmp(&rn),
            (Ok(_), Err(_)) => return Ordering::Less,
            (Err(_), Ok(_)) => return Ordering::Greater,
            (Err(_), Err(_)) => return lp.cmp(rp),
        }
    }
    Ordering::Equal
}

fn parse_representation(types: Option<&str>) -> Option<Representation> {
    match types {
        None => Some(Representation::Legacy),
        Some("") => None,
        Some(raw) => {
            if raw.split(',').any(|t| !is_valid_model_type(t)) {
                None
            } else {
                Some(Representation::Typed)
            }
        }
    }
}

fn extract_pi_user_agent_version(user_agent: Option<&str>) -> Option<String> {
    let ua = user_agent?;
    let rest = ua.strip_prefix("pi/")?;
    let version = rest.split(|c: char| c.is_whitespace() || c == '(' || c == ')').next()?;
    if !version.is_empty() && parse_pi_version(version).is_some() {
        Some(version.to_owned())
    } else {
        None
    }
}

fn parse_model_catalog_request(raw_url: &str, user_agent: Option<&str>) -> CatalogRequest {
    let mut url = Url::parse(raw_url).unwrap();
    let types_param = url
        .query_pairs()
        .find(|(k, _)| k == "types")
        .map(|(_, v)| v.into_owned());
    let Some(representation) = parse_representation(types_param.as_deref()) else {
        return CatalogRequest::Invalid {
            error: "Invalid model types.".to_owned(),
        };
    };

    let pi_version = url
        .query_pairs()
        .find(|(k, _)| k == "pi-version")
        .map(|(_, v)| v.into_owned());
    match pi_version {
        None => {
            if let Some(ua_ver) = extract_pi_user_agent_version(user_agent) {
                url.query_pairs_mut().append_pair("pi-version", &ua_ver);
                return CatalogRequest::Redirect {
                    location: url.to_string(),
                };
            }
            CatalogRequest::Catalog {
                pi_version: None,
                representation,
            }
        }
        Some(ver) => {
            if parse_pi_version(&ver).is_none() {
                CatalogRequest::Invalid {
                    error: "Invalid Pi version.".to_owned(),
                }
            } else {
                CatalogRequest::Catalog {
                    pi_version: Some(ver),
                    representation,
                }
            }
        }
    }
}

fn get_model_catalog_provider_key(revision: &str, provider: &str, representation: Representation) -> String {
    let suffix = match representation {
        Representation::Typed => ".all.json",
        Representation::Legacy => ".json",
    };
    format!("{MODEL_CATALOG_PREFIX}/revisions/{revision}/providers/{provider}{suffix}")
}

fn parse_model_catalog_index(value: &Value) -> CatalogIndex {
    let obj = value.as_object().expect("valid index object");
    assert_eq!(
        obj.get("schemaVersion").and_then(Value::as_u64),
        Some(MODEL_CATALOG_SCHEMA_VERSION)
    );
    let default_revision = obj
        .get("defaultRevision")
        .and_then(Value::as_str)
        .filter(|r| is_valid_revision(r))
        .expect("valid defaultRevision")
        .to_owned();
    let raw_catalogs = obj.get("catalogs").and_then(Value::as_array).expect("catalogs array");
    assert!(!raw_catalogs.is_empty());
    let mut catalogs = Vec::new();
    for entry in raw_catalogs {
        let min_ver = entry
            .get("minimumPiVersion")
            .and_then(Value::as_str)
            .filter(|v| parse_pi_version(v).is_some())
            .expect("valid minimumPiVersion")
            .to_owned();
        let rev = entry
            .get("revision")
            .and_then(Value::as_str)
            .filter(|r| is_valid_revision(r))
            .expect("valid revision")
            .to_owned();
        catalogs.push(CatalogIndexEntry {
            minimum_pi_version: min_ver,
            revision: rev,
        });
    }
    assert!(catalogs.iter().any(|c| c.revision == default_revision));
    CatalogIndex {
        _schema_version: MODEL_CATALOG_SCHEMA_VERSION,
        default_revision,
        catalogs,
    }
}

fn select_model_catalog<'a>(index: &'a CatalogIndex, pi_version: Option<&str>) -> Option<&'a CatalogIndexEntry> {
    let Some(ver) = pi_version else {
        return index.catalogs.iter().find(|c| c.revision == index.default_revision);
    };
    let req_ver = parse_pi_version(ver)?;
    let mut selected: Option<(&CatalogIndexEntry, (u64, u64, u64, String))> = None;
    for catalog in &index.catalogs {
        let min_ver = parse_pi_version(&catalog.minimum_pi_version)?;
        if compare_pi_versions(&min_ver, &req_ver) <= std::cmp::Ordering::Equal
            && selected
                .as_ref()
                .is_none_or(|(_, cur)| compare_pi_versions(&min_ver, cur) == std::cmp::Ordering::Greater)
        {
            selected = Some((catalog, min_ver));
        }
    }
    selected.map(|(c, _)| c)
}

#[tokio::test]
async fn negotiates_the_catalog_for_its_version_and_reaches_the_openrouter_api() {
    let legacy_revision = format!("sha256-{}", "a".repeat(64));
    let mixed_api_revision = format!("sha256-{}", "b".repeat(64));
    let model_id = "anthropic/claude-sonnet-5";
    let last_modified = "Tue, 01 Jan 2099 00:00:00 GMT";

    let legacy_model = json!({
        "id": model_id,
        "name": "Claude Sonnet 5",
        "provider": "openrouter",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 },
        "contextWindow": 200_000,
        "maxTokens": 64_000,
        "api": "openai-completions",
        "baseUrl": "https://openrouter.ai/api/v1"
    });
    let mixed_api_model = json!({
        "id": model_id,
        "name": "Claude Sonnet 5",
        "provider": "openrouter",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 },
        "contextWindow": 200_000,
        "maxTokens": 64_000,
        "api": "anthropic-messages",
        "baseUrl": "https://openrouter.ai/api"
    });
    let mut mixed_typed_model = mixed_api_model.clone();
    mixed_typed_model["type"] = Value::String("chat".to_owned());

    let index_json = json!({
        "schemaVersion": 1,
        "defaultRevision": mixed_api_revision,
        "catalogs": [
            { "minimumPiVersion": "0.80.7", "revision": legacy_revision },
            { "minimumPiVersion": "0.85.0", "revision": mixed_api_revision }
        ]
    });

    let mut objects: IndexMap<String, Value> = IndexMap::new();
    objects.insert(MODEL_CATALOG_INDEX_KEY.to_owned(), index_json.clone());
    objects.insert(
        get_model_catalog_provider_key(&legacy_revision, "openrouter", Representation::Legacy),
        json!({ model_id: legacy_model }),
    );
    objects.insert(
        get_model_catalog_provider_key(&mixed_api_revision, "openrouter", Representation::Legacy),
        json!({ model_id: mixed_api_model }),
    );
    objects.insert(
        get_model_catalog_provider_key(&mixed_api_revision, "openrouter", Representation::Typed),
        json!([mixed_typed_model]),
    );
    let objects = Arc::new(objects);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let catalog_base_url = format!("http://{addr}");

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_srv = requests.clone();
    let objects_srv = objects.clone();

    let server_task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let reqs = requests_srv.clone();
            let objs = objects_srv.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut total = 0usize;
                loop {
                    let n = socket.read(&mut buf[total..]).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") || total == buf.len() {
                        break;
                    }
                }
                let raw_req = String::from_utf8_lossy(&buf[..total]);
                let mut lines = raw_req.split("\r\n");
                let request_line = lines.next().unwrap_or("");
                let target = request_line.split_whitespace().nth(1).unwrap_or("/");
                let mut host = format!("{addr}");
                let mut user_agent: Option<String> = None;
                for line in lines {
                    if line.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        let lower = k.trim().to_ascii_lowercase();
                        if lower == "host" {
                            host = v.trim().to_owned();
                        } else if lower == "user-agent" {
                            user_agent = Some(v.trim().to_owned());
                        }
                    }
                }
                let full_url = format!("http://{host}{target}");
                let parsed_url = Url::parse(&full_url).unwrap();
                let path_and_query = match parsed_url.query() {
                    Some(q) => format!("{}?{q}", parsed_url.path()),
                    None => parsed_url.path().to_owned(),
                };
                reqs.lock().unwrap().push(path_and_query);

                let provider = parsed_url
                    .path()
                    .strip_prefix("/api/models/providers/")
                    .filter(|rest| !rest.contains('/') && !rest.is_empty());
                let catalog_req = parse_model_catalog_request(&full_url, user_agent.as_deref());

                let resp_bytes = match (provider, catalog_req) {
                    (None, _) => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
                    }
                    (Some(_), CatalogRequest::Redirect { location }) => format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .into_bytes(),
                    (Some(_), CatalogRequest::Invalid { error }) => format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",
                        error.len()
                    )
                    .into_bytes(),
                    (Some(prov), CatalogRequest::Catalog { pi_version, representation }) => {
                        let idx = parse_model_catalog_index(objs.get(MODEL_CATALOG_INDEX_KEY).unwrap());
                        let selected = select_model_catalog(&idx, pi_version.as_deref());
                        let body_opt = selected.and_then(|cat| {
                            objs.get(&get_model_catalog_provider_key(&cat.revision, prov, representation))
                                .or_else(|| {
                                    objs.get(&get_model_catalog_provider_key(&cat.revision, prov, Representation::Legacy))
                                })
                                .map(|body| (cat.revision.clone(), stringify(body)))
                        });
                        match body_opt {
                            Some((rev, body)) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nLast-Modified: {last_modified}\r\nX-Pi-Model-Catalog-Revision: {rev}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .into_bytes(),
                            None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                        }
                    }
                };
                let _ = socket.write_all(&resp_bytes).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(InMemoryCredentialStore::new())),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        models_path: Some(None),
        catalog_base_url: Some(catalog_base_url),
        refresh_on_create: Some(false),
        allow_model_network: Some(true),
        ..Default::default()
    })
    .await
    .unwrap();

    runtime
        .set_runtime_api_key("openrouter", "test-key", None)
        .await
        .unwrap();
    let refresh = runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(true),
            force: Some(true),
            providers: Some(vec!["openrouter".to_owned()]),
            signal: None,
        }))
        .await
        .unwrap();
    assert!(
        refresh.errors.is_empty(),
        "unexpected refresh errors: {:?}",
        refresh.errors
    );

    let catalog_url = "/api/models/providers/openrouter?types=chat%2Cimage%2Cclassifier";
    assert_eq!(
        *requests.lock().unwrap(),
        vec![catalog_url.to_owned(), format!("{catalog_url}&pi-version={}", *VERSION),]
    );

    let parsed_index = parse_model_catalog_index(&index_json);
    let expected = if select_model_catalog(&parsed_index, Some(*VERSION)).is_some_and(|c| c.revision == legacy_revision)
    {
        &legacy_model
    } else {
        &mixed_api_model
    };
    let model = runtime.get_model("openrouter", model_id).unwrap();
    assert_eq!(model.api, expected["api"].as_str().unwrap());
    assert_eq!(model.base_url, expected["baseUrl"].as_str().unwrap());

    let captured_url = Arc::new(Mutex::new(None::<Url>));
    let captured_clone = captured_url.clone();
    let _fetch_guard = mock_fetch(move |req| {
        let cap = captured_clone.clone();
        Box::pin(async move {
            let url = Url::parse(&req.url).unwrap();
            *cap.lock().unwrap() = Some(url);
            Err(Error::msg("Provider request captured"))
        })
    });

    let _ = runtime
        .complete_simple(
            &model,
            &Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hello".to_owned()),
                    timestamp: 0,
                })],
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: pi_ai::types::StreamOptions {
                        api_key: Some("test-key".to_owned()),
                        max_retries: Some(0),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .await;

    let observed_path = captured_url.lock().unwrap().as_ref().map(|u| u.path().to_owned());
    let expected_path = if expected["api"].as_str() == Some("anthropic-messages") {
        "/api/v1/messages"
    } else {
        "/api/v1/chat/completions"
    };
    assert_eq!(observed_path.as_deref(), Some(expected_path));

    server_task.abort();
}
