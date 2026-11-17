//! Port of packages/coding-agent/src/extensions/llama/client.ts

use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::fetch::{Body, Headers, Request, fetch};
use pi_js::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlamaModelStatus {
    Unloaded,
    Loading,
    Loaded,
    Downloading,
    Sleeping,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlamaProgressRange {
    pub done: i64,
    pub total: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaModelStatusInfo {
    pub value: LlamaModelStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<IndexMap<String, LlamaProgressRange>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaModelArchitecture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaModelMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx_train: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ftype: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    pub status: LlamaModelStatusInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<LlamaModelArchitecture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<LlamaModelMeta>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LlamaServerProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_autoload: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelEvent {
    pub model: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlamaProgress {
    pub message: String,
    pub ratio: Option<f64>,
    pub detail: Option<String>,
}

pub type OnLlamaProgressFn = Arc<dyn Fn(LlamaProgress) + Send + Sync>;

fn error_message(payload: Option<&Value>, fallback: &str) -> String {
    payload
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("error"))
        .and_then(|e| e.as_object())
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| fallback.to_string())
}

fn is_model_info(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let has_id = obj.get("id").is_some_and(Value::is_string);
    let has_status = obj
        .get("status")
        .and_then(Value::as_object)
        .and_then(|s| s.get("value"))
        .is_some_and(Value::is_string);
    has_id && has_status
}

fn parse_load_progress(data: Option<&Value>) -> Option<LlamaProgress> {
    let progress = data?.as_object()?.get("progress")?.as_object()?;
    let stage = progress
        .get("current")
        .or_else(|| progress.get("stage"))
        .and_then(Value::as_str)
        .map(String::from);
    let stages: Vec<&str> = progress
        .get("stages")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let stage_ratio = progress.get("value").and_then(Value::as_f64).map(|v| v.clamp(0.0, 1.0));
    let mut ratio = stage_ratio;
    if let Some(ref st) = stage
        && !stages.is_empty()
        && let Some(idx) = stages.iter().position(|s| *s == st)
    {
        ratio = Some((idx as f64 + stage_ratio.unwrap_or(0.0)) / stages.len() as f64);
    }
    Some(LlamaProgress {
        message: match stage {
            Some(st) => format!("Loading {}", st.replace('_', " ")),
            None => "Loading model".to_string(),
        },
        ratio,
        detail: None,
    })
}

fn parse_download_progress(data: Option<&Value>) -> Option<LlamaProgress> {
    let obj = data?.as_object()?;
    let files = obj.get("progress").and_then(Value::as_object).unwrap_or(obj);
    let mut done: i64 = 0;
    let mut total: i64 = 0;
    for value in files.values() {
        let Some(entry) = value.as_object() else {
            continue;
        };
        if let (Some(d), Some(t)) = (
            entry.get("done").and_then(Value::as_f64),
            entry.get("total").and_then(Value::as_f64),
        ) {
            done = done.saturating_add(d as i64);
            total = total.saturating_add(t as i64);
        }
    }
    if total <= 0 {
        return None;
    }
    Some(LlamaProgress {
        message: "Downloading model".to_string(),
        ratio: Some(done as f64 / total as f64),
        detail: Some(format!(
            "{} / {}",
            format_bytes(done as u64),
            format_bytes(total as u64)
        )),
    })
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = units[0];
    for next_unit in &units[1..] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next_unit;
    }
    if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

pub fn normalize_llama_server_url(value: &str) -> Result<String> {
    let mut url =
        url::Url::parse(pi_js::str16::trim(value)).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(Error::msg("Server URL must use http or https"));
    }
    url.set_fragment(None);
    url.set_query(None);
    let mut path = url.path().trim_end_matches('/').to_string();
    if let Some(stripped) = path.strip_suffix("/v1") {
        path = stripped.to_string();
    }
    if path.is_empty() {
        url.set_path("/");
    } else {
        url.set_path(&path);
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

pub fn llama_inference_url(server_url: &str) -> Result<String> {
    Ok(format!("{}/v1", normalize_llama_server_url(server_url)?))
}

#[derive(Clone, Debug)]
pub struct LlamaClient {
    pub server_url: String,
    api_key: Option<String>,
}

#[derive(Default)]
pub struct ListOptions {
    pub reload: bool,
    pub signal: Option<AbortSignal>,
}

#[derive(Default)]
pub struct PropsOptions {
    pub model: Option<String>,
    pub signal: Option<AbortSignal>,
}

impl LlamaClient {
    pub fn new(server_url: &str, api_key: Option<&str>) -> Result<Self> {
        Ok(Self {
            server_url: normalize_llama_server_url(server_url)?,
            api_key: api_key.map(String::from),
        })
    }

    async fn send_request(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        signal: Option<&AbortSignal>,
    ) -> Result<pi_js::fetch::Response> {
        let mut headers = Headers::new();
        if body.is_some() {
            headers.set("Content-Type", "application/json");
        }
        if let Some(ref key) = self.api_key
            && !key.is_empty()
        {
            headers.set("Authorization", &format!("Bearer {key}"));
        }
        let mut req = Request::new(method.to_string(), format!("{}{path}", self.server_url)).with_headers(headers);
        if let Some(b) = body {
            req = req.body(Body::Text(b));
        }
        if let Some(sig) = signal {
            req = req.signal(sig.clone());
        }
        match fetch(req).await {
            Ok(resp) => Ok(resp),
            Err(err) => {
                if let Some(sig) = signal
                    && sig.aborted()
                {
                    return Err(err);
                }
                Err(Error::msg(format!(
                    "Could not reach llama.cpp server at {}: {}",
                    self.server_url,
                    err.message()
                )))
            }
        }
    }

    async fn request_json(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        caller_signal: Option<AbortSignal>,
    ) -> Result<Value> {
        let timeout = AbortSignal::timeout(15_000);
        let combined = match caller_signal {
            Some(ref sig) => AbortSignal::any(&[sig.clone(), timeout]),
            None => timeout,
        };
        let response = self.send_request(method, path, body, Some(&combined)).await?;
        let ok = response.ok();
        let status = response.status();
        let payload = response.json().await.ok();
        if !ok {
            return Err(Error::msg(error_message(
                payload.as_ref(),
                &format!("llama.cpp returned HTTP {status}"),
            )));
        }
        Ok(payload.unwrap_or(Value::Null))
    }

    pub async fn list(&self, options: ListOptions) -> Result<Vec<LlamaModelInfo>> {
        let path = if options.reload { "/models?reload=1" } else { "/models" };
        let payload = self.request_json("GET", path, None, options.signal).await?;
        let Some(data) = payload
            .as_object()
            .and_then(|o| o.get("data"))
            .and_then(Value::as_array)
        else {
            return Err(Error::msg("llama.cpp returned an invalid model catalog"));
        };
        if !data.iter().all(is_model_info) {
            return Err(Error::msg("Server is not running in llama.cpp router mode"));
        }
        let mut models = Vec::with_capacity(data.len());
        for item in data {
            let parsed: LlamaModelInfo = pi_js::json::parse_as(&pi_js::json::stringify(item))
                .map_err(|_| Error::msg("Server is not running in llama.cpp router mode"))?;
            models.push(parsed);
        }
        Ok(models)
    }

    pub async fn props(&self, options: PropsOptions) -> Result<LlamaServerProps> {
        let path = match options.model {
            Some(ref model) => {
                let mut ser = url::form_urlencoded::Serializer::new(String::new());
                ser.append_pair("model", model);
                ser.append_pair("autoload", "false");
                format!("/props?{}", ser.finish())
            }
            None => "/props".to_string(),
        };
        let payload = self.request_json("GET", &path, None, options.signal).await?;
        let Some(obj) = payload.as_object() else {
            return Ok(LlamaServerProps::default());
        };
        Ok(LlamaServerProps {
            models_autoload: obj.get("models_autoload").and_then(Value::as_bool),
            chat_template: obj.get("chat_template").and_then(Value::as_str).map(String::from),
        })
    }

    pub async fn load(&self, model: &str, signal: Option<AbortSignal>) -> Result<()> {
        self.request_json(
            "POST",
            "/models/load",
            Some(pi_js::json::stringify(&json!({ "model": model }))),
            signal,
        )
        .await?;
        Ok(())
    }

    pub async fn unload(&self, model: &str, signal: Option<AbortSignal>) -> Result<()> {
        self.request_json(
            "POST",
            "/models/unload",
            Some(pi_js::json::stringify(&json!({ "model": model }))),
            signal,
        )
        .await?;
        Ok(())
    }

    pub async fn unload_and_wait(&self, model: &str, signal: Option<AbortSignal>) -> Result<()> {
        self.unload(model, signal.clone()).await?;
        loop {
            let entry = self
                .list(ListOptions {
                    reload: false,
                    signal: signal.clone(),
                })
                .await?
                .into_iter()
                .find(|c| c.id == model);
            if entry.is_none_or(|e| e.status.value == LlamaModelStatus::Unloaded) {
                return Ok(());
            }
            pi_js::time::sleep(100, signal.as_ref()).await?;
        }
    }

    pub async fn download(&self, model: &str, signal: Option<AbortSignal>) -> Result<()> {
        self.request_json(
            "POST",
            "/models",
            Some(pi_js::json::stringify(&json!({ "model": model }))),
            signal,
        )
        .await?;
        Ok(())
    }

    pub async fn watch<F>(&self, mut on_event: F, signal: Option<AbortSignal>) -> Result<()>
    where
        F: FnMut(LlamaModelEvent) + Send,
    {
        let response = self.send_request("GET", "/models/sse", None, signal.as_ref()).await?;
        if !response.ok() {
            let status = response.status();
            response.cancel_body();
            return Err(Error::msg(format!("llama.cpp SSE returned HTTP {status}")));
        }
        let mut stream = response.body_stream()?;
        let mut raw_buf: Vec<u8> = Vec::new();
        let mut text_buf = String::new();
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res?;
            raw_buf.extend_from_slice(&chunk);
            let valid_up_to = match std::str::from_utf8(&raw_buf) {
                Ok(s) => s.len(),
                Err(e) => e.valid_up_to(),
            };
            if valid_up_to > 0 {
                let s = String::from_utf8_lossy(&raw_buf[..valid_up_to]);
                text_buf.push_str(&s.replace("\r\n", "\n"));
                raw_buf.drain(..valid_up_to);
            }
            while let Some(boundary) = text_buf.find("\n\n") {
                let frame = text_buf[..boundary].to_string();
                text_buf.drain(..boundary + 2);
                let data = frame
                    .split('\n')
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(pi_js::str16::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !data.is_empty()
                    && let Ok(event) = pi_js::json::parse_as::<LlamaModelEvent>(&data)
                {
                    on_event(event);
                }
            }
        }
        Ok(())
    }

    pub async fn load_and_wait(
        &self,
        model: &str,
        on_progress: OnLlamaProgressFn,
        signal: Option<AbortSignal>,
    ) -> Result<LlamaModelInfo> {
        let watcher = Arc::new(AbortController::new());
        let watcher_signal = watcher.signal();
        let _unlink = signal.as_ref().map(|sig| {
            let w = Arc::clone(&watcher);
            sig.on_abort(move |reason| w.abort(Some(reason.clone())))
        });

        let event_loaded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let event_error = Arc::new(std::sync::Mutex::new(None::<String>));

        let client_watch = self.clone();
        let model_watch = model.to_string();
        let ev_loaded_w = Arc::clone(&event_loaded);
        let ev_err_w = Arc::clone(&event_error);
        let on_prog_w = Arc::clone(&on_progress);
        let w_sig = watcher_signal.clone();
        tokio::spawn(async move {
            let _ = client_watch
                .watch(
                    move |event| {
                        if event.model != model_watch {
                            return;
                        }
                        if event.event != "model_status" && event.event != "status_change" {
                            return;
                        }
                        let status = event
                            .data
                            .as_ref()
                            .and_then(|d| d.as_object())
                            .and_then(|o| o.get("status"))
                            .and_then(Value::as_str);
                        if status == Some("loaded") {
                            ev_loaded_w.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        if status == Some("unloaded") {
                            *ev_err_w.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some("Model failed to load".to_string());
                        }
                        if let Some(progress) = parse_load_progress(event.data.as_ref()) {
                            on_prog_w(progress);
                        }
                    },
                    Some(w_sig),
                )
                .await;
        });

        let res = async {
            self.load(model, signal.clone()).await?;
            on_progress(LlamaProgress {
                message: "Loading model".to_string(),
                ratio: None,
                detail: None,
            });
            loop {
                if let Some(ref sig) = signal
                    && sig.aborted()
                {
                    return Err(Error::js("AbortError", "Cancelled"));
                }
                let entry = self
                    .list(ListOptions {
                        reload: false,
                        signal: signal.clone(),
                    })
                    .await?
                    .into_iter()
                    .find(|c| c.id == model);
                if entry
                    .as_ref()
                    .is_some_and(|e| e.status.value == LlamaModelStatus::Loaded)
                    && let Some(e) = entry
                {
                    return Ok(e);
                }
                if event_loaded.load(std::sync::atomic::Ordering::SeqCst) && entry.is_none() {
                    return Ok(LlamaModelInfo {
                        id: model.to_string(),
                        aliases: None,
                        status: LlamaModelStatusInfo {
                            value: LlamaModelStatus::Loaded,
                            args: None,
                            failed: None,
                            exit_code: None,
                            progress: None,
                        },
                        architecture: None,
                        source: None,
                        meta: None,
                    });
                }
                let ev_err = event_error.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let failed = entry.as_ref().and_then(|e| e.status.failed).unwrap_or(false);
                if failed || ev_err.is_some() {
                    let msg = match entry.as_ref().and_then(|e| e.status.exit_code) {
                        Some(code) => format!("Model exited with code {code}"),
                        None => ev_err.unwrap_or_else(|| "Model failed to load".to_string()),
                    };
                    return Err(Error::msg(msg));
                }
                pi_js::time::sleep(250, signal.as_ref()).await?;
            }
        }
        .await;

        watcher.abort(Some(AbortReason::error("AbortError", "done")));
        res
    }

    pub async fn download_and_wait(
        &self,
        model: &str,
        on_progress: OnLlamaProgressFn,
        signal: Option<AbortSignal>,
    ) -> Result<Vec<LlamaModelInfo>> {
        let watcher = Arc::new(AbortController::new());
        let watcher_signal = watcher.signal();
        let _unlink = signal.as_ref().map(|sig| {
            let w = Arc::clone(&watcher);
            sig.on_abort(move |reason| w.abort(Some(reason.clone())))
        });

        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_downloading = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failure = Arc::new(std::sync::Mutex::new(None::<String>));

        let client_watch = self.clone();
        let model_watch = model.to_string();
        let fin_w = Arc::clone(&finished);
        let saw_w = Arc::clone(&saw_downloading);
        let fail_w = Arc::clone(&failure);
        let on_prog_w = Arc::clone(&on_progress);
        let w_sig = watcher_signal.clone();
        tokio::spawn(async move {
            let _ = client_watch
                .watch(
                    move |event| {
                        if event.model != model_watch {
                            return;
                        }
                        if event.event == "download_finished" {
                            fin_w.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        if event.event == "download_failed" {
                            *fail_w.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(error_message(event.data.as_ref(), "Download failed"));
                        }
                        if event.event == "download_progress" {
                            saw_w.store(true, std::sync::atomic::Ordering::SeqCst);
                            if let Some(progress) = parse_download_progress(event.data.as_ref()) {
                                on_prog_w(progress);
                            }
                        }
                    },
                    Some(w_sig),
                )
                .await;
        });

        let res = async {
            self.download(model, signal.clone()).await?;
            on_progress(LlamaProgress {
                message: "Downloading model".to_string(),
                ratio: None,
                detail: None,
            });
            let mut polls: usize = 0;
            loop {
                if let Some(ref sig) = signal
                    && sig.aborted()
                {
                    return Err(Error::js("AbortError", "Cancelled"));
                }
                if let Some(err) = failure.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    return Err(Error::msg(err));
                }
                let models = self
                    .list(ListOptions {
                        reload: false,
                        signal: signal.clone(),
                    })
                    .await?;
                polls += 1;
                let entry = models.iter().find(|c| c.id == model);
                if let Some(e) = entry
                    && e.status.value == LlamaModelStatus::Downloading
                {
                    saw_downloading.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(ref prog) = e.status.progress {
                        let val = pi_js::json::parse(&pi_js::json::stringify(prog)).ok();
                        if let Some(progress) = parse_download_progress(val.as_ref()) {
                            on_progress(progress);
                        }
                    }
                } else if finished.load(std::sync::atomic::Ordering::SeqCst)
                    || (entry.is_some() && (saw_downloading.load(std::sync::atomic::Ordering::SeqCst) || polls >= 2))
                {
                    return self
                        .list(ListOptions {
                            reload: true,
                            signal: signal.clone(),
                        })
                        .await;
                }
                pi_js::time::sleep(500, signal.as_ref()).await?;
            }
        }
        .await;

        watcher.abort(Some(AbortReason::error("AbortError", "done")));
        res
    }
}
