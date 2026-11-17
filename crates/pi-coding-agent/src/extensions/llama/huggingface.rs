//! Port of packages/coding-agent/src/extensions/llama/huggingface.ts

use std::cmp::Ordering;
use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Headers, Request, fetch};
use pi_js::{Error, Result};

const DEFAULT_HUGGING_FACE_URL: &str = "https://huggingface.co";

static QUANTIZATION_PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:^|[-_.])((?:UD-)?(?:IQ[0-9](?:_[A-Z0-9]+)+|Q[0-9](?:_[A-Z0-9]+)+|BF16|F16|F32|MXFP[0-9](?:_[A-Z0-9]+)*))$",
    )
    .expect("valid quantization regex")
});
static SHARD_SUFFIX_PATTERN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"-[0-9]{5}-of-[0-9]{5}$").expect("valid shard regex"));
static RATE_LIMIT_DELAY_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?:^|;)t=([0-9]+)").expect("valid ratelimit regex"));

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuggingFaceModel {
    pub id: String,
    pub downloads: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuggingFaceQuantization {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HuggingFaceGated {
    False,
    Auto,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HuggingFaceModelDetails {
    pub id: String,
    pub gated: HuggingFaceGated,
    pub quantizations: Vec<HuggingFaceQuantization>,
}

fn payload_error(payload: Option<&Value>, fallback: &str) -> String {
    payload
        .and_then(Value::as_object)
        .and_then(|o| o.get("error"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| fallback.to_string())
}

fn parse_rate_limit_delay(value: Option<&str>) -> Option<i64> {
    let val = value?;
    let caps = RATE_LIMIT_DELAY_RE.captures(val)?;
    caps.get(1)?.as_str().parse::<i64>().ok()
}

async fn read_token(path: &str) -> Option<String> {
    let content = pi_js::fs::promises::read_to_string(path).await.ok()?;
    let trimmed = pi_js::str16::trim(&content);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub async fn find_hugging_face_token(env: Option<&IndexMap<String, String>>) -> Option<String> {
    let get_env = |key: &str| -> Option<String> {
        match env {
            Some(map) => map.get(key).cloned(),
            None => pi_js::env::var(key),
        }
    };
    if let Some(from_env) = get_env("HF_TOKEN") {
        let trimmed = pi_js::str16::trim(&from_env);
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let mut paths = IndexSet::new();
    if let Some(p) = get_env("HF_TOKEN_PATH").filter(|s| !s.is_empty()) {
        paths.insert(p);
    }
    if let Some(home) = get_env("HF_HOME").filter(|s| !s.is_empty()) {
        paths.insert(pi_js::path::join(&[&home, "token"]));
    }
    if let Some(xdg) = get_env("XDG_CACHE_HOME").filter(|s| !s.is_empty()) {
        paths.insert(pi_js::path::join(&[&xdg, "huggingface", "token"]));
    }
    paths.insert(pi_js::path::join(&[
        &pi_js::env::home_dir(),
        ".cache",
        "huggingface",
        "token",
    ]));
    for path in paths {
        if let Some(token) = read_token(&path).await {
            return Some(token);
        }
    }
    None
}

#[derive(Clone, Debug)]
pub struct HuggingFaceClient {
    token: Option<String>,
    base_url: String,
}

impl HuggingFaceClient {
    pub fn new(token: Option<String>, base_url: Option<&str>) -> Self {
        let base = base_url
            .unwrap_or(DEFAULT_HUGGING_FACE_URL)
            .trim_end_matches('/')
            .to_string();
        Self { token, base_url: base }
    }

    async fn request(&self, path: &str, signal: Option<AbortSignal>) -> Result<Value> {
        let mut headers = Headers::new();
        if let Some(ref token) = self.token
            && !token.is_empty()
        {
            headers.set("Authorization", &format!("Bearer {token}"));
        }
        let timeout = AbortSignal::timeout(15_000);
        let combined = match signal {
            Some(sig) => AbortSignal::any(&[sig, timeout]),
            None => timeout,
        };
        let req = Request::new("GET".to_string(), format!("{}{path}", self.base_url))
            .with_headers(headers)
            .signal(combined);
        let response = fetch(req).await?;
        let ok = response.ok();
        let status = response.status();
        let retry_after = response.headers().get("retry-after");
        let ratelimit = response.headers().get("ratelimit");
        let payload = response.json().await.ok();
        if !ok {
            if status == 429 {
                let delay = retry_after
                    .as_deref()
                    .and_then(|s| s.parse::<i64>().ok())
                    .filter(|n| *n > 0)
                    .or_else(|| parse_rate_limit_delay(ratelimit.as_deref()));
                return Err(Error::msg(match delay {
                    Some(d) => format!("Hugging Face rate limit reached; retry in {d}s"),
                    None => "Hugging Face rate limit reached".to_string(),
                }));
            }
            let fallback = format!("Hugging Face returned HTTP {status}");
            return Err(Error::msg(payload_error(payload.as_ref(), &fallback)));
        }
        Ok(payload.unwrap_or(Value::Null))
    }

    pub async fn search(&self, query: &str, signal: Option<AbortSignal>) -> Result<Vec<HuggingFaceModel>> {
        let qs = {
            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            ser.append_pair("search", query);
            ser.append_pair("filter", "gguf");
            ser.append_pair("sort", "downloads");
            ser.append_pair("direction", "-1");
            ser.append_pair("limit", "20");
            ser.finish()
        };
        let payload = self.request(&format!("/api/models?{qs}"), signal).await?;
        let Some(arr) = payload.as_array() else {
            return Err(Error::msg("Hugging Face returned invalid search results"));
        };
        let mut out = Vec::new();
        for val in arr {
            let Some(obj) = val.as_object() else {
                continue;
            };
            let Some(id) = obj.get("id").and_then(Value::as_str) else {
                continue;
            };
            let downloads = obj
                .get("downloads")
                .and_then(Value::as_f64)
                .map(|n| n as i64)
                .unwrap_or(0);
            out.push(HuggingFaceModel {
                id: id.to_string(),
                downloads,
            });
        }
        Ok(out)
    }

    pub async fn details(&self, id: &str, signal: Option<AbortSignal>) -> Result<HuggingFaceModelDetails> {
        let encoded_id = id
            .split('/')
            .map(pi_js::uri::encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        let payload = self
            .request(&format!("/api/models/{encoded_id}?blobs=true"), signal)
            .await?;
        let Some(model) = payload.as_object() else {
            return Err(Error::msg("Hugging Face returned invalid model details"));
        };

        let mut sizes: IndexMap<String, (i64, bool)> = IndexMap::new();
        if let Some(siblings) = model.get("siblings").and_then(Value::as_array) {
            for val in siblings {
                let Some(file) = val.as_object() else {
                    continue;
                };
                let Some(rfilename) = file.get("rfilename").and_then(Value::as_str) else {
                    continue;
                };
                if !rfilename.to_ascii_lowercase().ends_with(".gguf") {
                    continue;
                }
                let filename = rfilename.rsplit('/').next().unwrap_or(rfilename);
                if filename.to_ascii_lowercase().starts_with("mmproj") {
                    continue;
                }
                let without_ext = &filename[..filename.len() - 5];
                let stem = SHARD_SUFFIX_PATTERN.replace(without_ext, "");
                let Some(caps) = QUANTIZATION_PATTERN.captures(&stem) else {
                    continue;
                };
                let Some(quant_match) = caps.get(1) else {
                    continue;
                };
                let quantization = quant_match.as_str().to_ascii_uppercase();
                let entry = sizes.entry(quantization).or_insert((0, true));
                if let Some(sz) = file.get("size").and_then(Value::as_f64) {
                    entry.0 += sz as i64;
                } else {
                    entry.1 = false;
                }
            }
        }

        let mut quantizations: Vec<HuggingFaceQuantization> = sizes
            .into_iter()
            .map(|(name, (total, complete))| HuggingFaceQuantization {
                name,
                size: if complete { Some(total) } else { None },
            })
            .collect();

        quantizations.sort_by(|left, right| {
            if left.name == "Q4_K_M" && right.name != "Q4_K_M" {
                return Ordering::Less;
            }
            if right.name == "Q4_K_M" && left.name != "Q4_K_M" {
                return Ordering::Greater;
            }
            let lsize = left.size.unwrap_or(i64::MAX);
            let rsize = right.size.unwrap_or(i64::MAX);
            lsize
                .cmp(&rsize)
                .then_with(|| pi_js::intl::locale_compare(&left.name, &right.name))
        });

        let resolved_id = model
            .get("id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| id.to_string());
        let gated = match model.get("gated").and_then(Value::as_str) {
            Some("auto") => HuggingFaceGated::Auto,
            Some("manual") => HuggingFaceGated::Manual,
            _ => HuggingFaceGated::False,
        };

        Ok(HuggingFaceModelDetails {
            id: resolved_id,
            gated,
            quantizations,
        })
    }
}
