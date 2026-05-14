use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use pi_agent_core::config::{AuthStorage, StoredCredential};
use pi_agent_core::messages::{
    AgentMessage, AssistantContentBlock, TextContent, ToolCallContent, Usage, UserContentBlock,
};
use pi_agent_core::models::{
    CompletionRequest, CompletionResponse, CompletionToolCall, ModelProvider,
};
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
const OAUTH_SCOPE: &str = "openid profile email offline_access";
const CLAIM_PATH: &str = "https://api.openai.com/auth";

#[derive(Debug, Clone)]
pub struct OpenAiCodexProvider {
    client: Client,
    auth_storage: AuthStorage,
}

impl OpenAiCodexProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder().build()?,
            auth_storage: AuthStorage::new()?,
        })
    }

    pub async fn login(&self) -> Result<()> {
        let verifier = random_token();
        let challenge = pkce_challenge(&verifier);
        let state = random_token();
        let mut url = reqwest::Url::parse(AUTHORIZE_URL)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", OAUTH_SCOPE)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", "pi-coding-agent");

        let callback = wait_for_callback(state.clone());
        let _ = webbrowser::open(url.as_str());
        println!("Complete login in your browser.");
        println!("If the browser does not open, visit:\n{url}");

        let code = match tokio::time::timeout(std::time::Duration::from_secs(180), callback).await {
            Ok(Ok(code)) => code,
            _ => {
                print!("Paste the full redirect URL or code: ");
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                parse_code_from_input(&input, &state)?
            }
        };

        let token = self.exchange_code(&code, &verifier).await?;
        self.auth_storage.save(
            "openai-codex",
            StoredCredential::OAuth {
                access: token.access.clone(),
                refresh: token.refresh,
                expires: token.expires,
                account_id: token.account_id,
            },
        )?;
        println!(
            "Stored OpenAI Codex credentials in {}",
            self.auth_storage.path().display()
        );
        Ok(())
    }

    pub fn logout(&self) -> Result<()> {
        self.auth_storage.remove("openai-codex")
    }

    async fn credential(&self) -> Result<OAuthToken> {
        match self.auth_storage.load("openai-codex")? {
            Some(StoredCredential::OAuth {
                access,
                refresh,
                expires,
                account_id,
            }) => {
                let mut token = OAuthToken {
                    access,
                    refresh,
                    expires,
                    account_id,
                };
                if token.expires <= Utc::now().timestamp_millis() + 60_000 {
                    token = self.refresh_token(&token.refresh).await?;
                    self.auth_storage.save(
                        "openai-codex",
                        StoredCredential::OAuth {
                            access: token.access.clone(),
                            refresh: token.refresh.clone(),
                            expires: token.expires,
                            account_id: token.account_id.clone(),
                        },
                    )?;
                }
                Ok(token)
            }
            Some(StoredCredential::ApiKey { .. }) | None => {
                Err(anyhow!("not authenticated; run `pi login` first"))
            }
        }
    }

    async fn exchange_code(&self, code: &str, verifier: &str) -> Result<OAuthToken> {
        let json = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", REDIRECT_URI),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        oauth_token_from_value(json)
    }

    async fn refresh_token(&self, refresh: &str) -> Result<OAuthToken> {
        let json = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", refresh),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        oauth_token_from_value(json)
    }
}

#[async_trait]
impl ModelProvider for OpenAiCodexProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        match request.model.api.as_str() {
            "openai-completions" => self.complete_openai_completions(request).await,
            "openai-codex-responses" => self.complete_codex_responses(request).await,
            other => Err(anyhow!("unsupported model api {other}")),
        }
    }
}

impl OpenAiCodexProvider {
    async fn complete_codex_responses(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse> {
        let token = self.credential().await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.access))?,
        );
        headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_str(&token.account_id)?,
        );
        headers.insert("originator", HeaderValue::from_static("pi-coding-agent"));
        headers.insert(
            "OpenAI-Beta",
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("pi-coding-agent"));

        let base_url = request
            .model
            .base_url
            .clone()
            .unwrap_or_else(|| "https://chatgpt.com/backend-api".to_string());
        let url = format!("{}/codex/responses", base_url.trim_end_matches('/'));
        let body = json!({
            "model": request.model.id,
            "store": false,
            "stream": true,
            "instructions": request.system_prompt,
            "input": convert_messages(&request.messages),
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "text": {"verbosity": "medium"},
            "include": ["reasoning.encrypted_content"],
            "tools": request.tools.iter().map(|tool| json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": false
            })).collect::<Vec<_>>()
        });

        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let response_text = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "Codex request failed with {status}: {response_text}"
            ));
        }
        parse_codex_sse(&response_text)
    }

    async fn complete_openai_completions(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static("pi-coding-agent"));
        let api_key = request
            .model
            .api_key
            .clone()
            .filter(|key| !key.is_empty())
            .or(self.auth_storage.api_key(&request.model.provider)?);
        if let Some(api_key) = api_key.as_deref() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {api_key}"))?,
            );
        }
        if let Some(extra_headers) = &request.model.headers {
            for (key, value) in extra_headers {
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(key.as_bytes())?,
                    HeaderValue::from_str(value)?,
                );
            }
        }

        let base_url =
            request.model.base_url.clone().ok_or_else(|| {
                anyhow!("model {} is missing baseUrl", request.model.canonical_id())
            })?;
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let body = json!({
            "model": request.model.id,
            "messages": convert_messages_to_chat_completions(&request.system_prompt, &request.messages),
            "tools": request.tools.iter().map(|tool| json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })).collect::<Vec<_>>(),
            "tool_choice": "auto",
            "stream": false,
        });

        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let response_text = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "OpenAI-compatible request failed with {status}: {response_text}"
            ));
        }
        let response = serde_json::from_str::<Value>(&response_text)?;
        parse_chat_completion(response)
    }
}

#[derive(Debug, Clone)]
struct OAuthToken {
    access: String,
    refresh: String,
    expires: i64,
    account_id: String,
}

fn oauth_token_from_value(value: Value) -> Result<OAuthToken> {
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .context("missing access_token")?
        .to_string();
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .context("missing refresh_token")?
        .to_string();
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_i64)
        .context("missing expires_in")?;
    let account_id = extract_account_id(&access)?;
    Ok(OAuthToken {
        access,
        refresh,
        expires: Utc::now().timestamp_millis() + (expires_in * 1000),
        account_id,
    })
}

fn extract_account_id(access_token: &str) -> Result<String> {
    let parts = access_token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(anyhow!("invalid access token"));
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .context("failed to decode token payload")?;
    let json: Value = serde_json::from_slice(&payload)?;
    json.get(CLAIM_PATH)
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("missing chatgpt account id")
}

fn random_token() -> String {
    Uuid::new_v4().simple().to_string()
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

async fn wait_for_callback(state: String) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:1455").await?;
    let (mut stream, _) = listener.accept().await?;
    let mut buffer = vec![0; 4096];
    let size = stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..size]);
    let first_line = request.lines().next().unwrap_or_default();
    let path = first_line
        .split_whitespace()
        .nth(1)
        .context("missing callback path")?;
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{path}"))?;
    let incoming_state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string());
    let body = if incoming_state.as_deref() == Some(state.as_str()) && code.is_some() {
        "<html><body>Authentication complete. You can close this tab.</body></html>"
    } else {
        "<html><body>Authentication failed.</body></html>"
    };
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        )
        .await?;
    let code = code.context("missing authorization code")?;
    if incoming_state.as_deref() != Some(state.as_str()) {
        return Err(anyhow!("state mismatch"));
    }
    Ok(code)
}

fn parse_code_from_input(input: &str, expected_state: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let url = reqwest::Url::parse(trimmed)?;
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .context("missing code parameter")?;
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        if let Some(state) = state {
            if state != expected_state {
                return Err(anyhow!("state mismatch"));
            }
        }
        Ok(code)
    } else {
        Ok(trimmed.to_string())
    }
}

fn convert_messages(messages: &[AgentMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| match message {
            AgentMessage::User { content, .. } => json!({
                "role": "user",
                "content": content.iter().map(|block| match block {
                    UserContentBlock::Text(text) => json!({"type": "input_text", "text": text.text}),
                    UserContentBlock::Image(image) => json!({
                        "type": "input_image",
                        "detail": "auto",
                        "image_url": format!("data:{};base64,{}", image.mime_type, image.data)
                    }),
                }).collect::<Vec<_>>()
            }),
            AgentMessage::Assistant {
                content,
                provider: _,
                model: _,
                api: _,
                usage: _,
                stop_reason: _,
                error_message: _,
                timestamp: _,
            } => {
                let mut output = Vec::new();
                for block in content {
                    match block {
                        AssistantContentBlock::Text(text) => output.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text.text, "annotations": []}],
                            "status": "completed",
                            "id": format!("msg_{}", Uuid::new_v4().simple()),
                        })),
                        AssistantContentBlock::ToolCall(call) => {
                            let mut parts = call.id.split('|');
                            let call_id = parts.next().unwrap_or(&call.id);
                            let item_id = parts.next().unwrap_or("fc_local");
                            output.push(json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "id": item_id,
                                "name": call.name,
                                "arguments": call.arguments.to_string()
                            }));
                        }
                        AssistantContentBlock::Thinking { .. } => {}
                    }
                }
                json!(output)
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                let call_id = tool_call_id.split('|').next().unwrap_or(tool_call_id);
                let text = content
                    .iter()
                    .map(|block| match block {
                        UserContentBlock::Text(text) => text.text.clone(),
                        UserContentBlock::Image(image) => format!("[image:{}]", image.mime_type),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": text
                })
            }
            AgentMessage::CompactionSummary { summary, .. } => json!({
                "role": "developer",
                "content": [{"type": "input_text", "text": summary}]
            }),
            AgentMessage::BranchSummary { summary, .. } => json!({
                "role": "user",
                "content": [{"type": "input_text", "text": format!("The following is a summary of a branch that this conversation came back from:\n\n<summary>\n{}\n</summary>", summary)}]
            }),
        })
        .flat_map(|value| match value {
            Value::Array(values) => values,
            other => vec![other],
        })
        .collect()
}

fn parse_codex_sse(text: &str) -> Result<CompletionResponse> {
    let mut assistant_blocks = Vec::new();
    let mut current_text = String::new();
    let mut current_tool: Option<(String, String, String)> = None;
    let mut tool_calls = Vec::new();
    let mut usage = Usage::default();
    let mut stop_reason = "stop".to_string();

    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data.trim() == "[DONE]" || data.trim().is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(data) {
            Ok(event) => event,
            Err(_) => continue,
        };
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    current_text.push_str(delta);
                }
            }
            "response.output_item.added" => {
                let item = event.get("item").cloned().unwrap_or_else(|| json!({}));
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    if !current_text.is_empty() {
                        assistant_blocks.push(AssistantContentBlock::Text(TextContent::new(
                            std::mem::take(&mut current_text),
                        )));
                    }
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("call")
                        .to_string();
                    let item_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("fc_local")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    current_tool = Some((format!("{call_id}|{item_id}"), name, String::new()));
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some((_, _, args)) = current_tool.as_mut() {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        args.push_str(delta);
                    }
                }
            }
            "response.function_call_arguments.done" => {
                if let Some((_, _, args)) = current_tool.as_mut() {
                    if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                        *args = arguments.to_string();
                    }
                }
            }
            "response.output_item.done" => {
                let item = event.get("item").cloned().unwrap_or_else(|| json!({}));
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        let text = item
                            .get("content")
                            .and_then(Value::as_array)
                            .map(|content| {
                                content
                                    .iter()
                                    .filter_map(|part| {
                                        part.get("text")
                                            .or_else(|| part.get("refusal"))
                                            .and_then(Value::as_str)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        if !text.is_empty() {
                            current_text = text;
                        }
                    }
                    Some("function_call") => {
                        let (id, name, args) = current_tool.take().unwrap_or_else(|| {
                            let call_id = item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or("call");
                            let item_id =
                                item.get("id").and_then(Value::as_str).unwrap_or("fc_local");
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let args = item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}")
                                .to_string();
                            (format!("{call_id}|{item_id}"), name, args)
                        });
                        let arguments = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                        assistant_blocks.push(AssistantContentBlock::ToolCall(
                            ToolCallContent::new(id.clone(), name.clone(), arguments.clone()),
                        ));
                        tool_calls.push(CompletionToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.done" => {
                let response = event.get("response").cloned().unwrap_or_else(|| json!({}));
                if let Some(response_usage) = response.get("usage") {
                    let input = response_usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    let cached = response_usage
                        .get("input_tokens_details")
                        .and_then(|details| details.get("cached_tokens"))
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    usage.input = input.saturating_sub(cached);
                    usage.cache_read = cached;
                    usage.output = response_usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    usage.total_tokens = response_usage
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(usage.input + usage.cache_read + usage.output);
                }
                if response.get("status").and_then(Value::as_str) == Some("incomplete") {
                    stop_reason = "max_output_tokens".to_string();
                }
            }
            "response.failed" | "error" => {
                return Err(anyhow!("Codex stream error: {event}"));
            }
            _ => {}
        }
    }

    if !current_text.is_empty() {
        assistant_blocks.push(AssistantContentBlock::Text(TextContent::new(current_text)));
    }

    Ok(CompletionResponse {
        assistant_blocks,
        tool_calls,
        usage,
        stop_reason,
    })
}

fn convert_messages_to_chat_completions(
    system_prompt: &str,
    messages: &[AgentMessage],
) -> Vec<Value> {
    let mut converted = vec![json!({"role": "system", "content": system_prompt})];
    for message in messages {
        match message {
            AgentMessage::User { content, .. } => converted.push(json!({
                "role": "user",
                "content": content.iter().map(|block| match block {
                    UserContentBlock::Text(text) => json!({"type": "text", "text": text.text}),
                    UserContentBlock::Image(image) => json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{};base64,{}", image.mime_type, image.data)}
                    }),
                }).collect::<Vec<_>>()
            })),
            AgentMessage::Assistant { content, .. } => {
                let text = content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let tool_calls = content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall(call) => Some(json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.arguments.to_string(),
                            }
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let mut assistant = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) }});
                if !tool_calls.is_empty() {
                    assistant["tool_calls"] = Value::Array(tool_calls);
                }
                converted.push(assistant);
            }
            AgentMessage::ToolResult { tool_call_id, content, .. } => converted.push(json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content.iter().map(|block| match block {
                    UserContentBlock::Text(text) => text.text.clone(),
                    UserContentBlock::Image(image) => format!("[image:{}]", image.mime_type),
                }).collect::<Vec<_>>().join("\n")
            })),
            AgentMessage::CompactionSummary { summary, .. } => converted.push(json!({
                "role": "system",
                "content": summary,
            })),
            AgentMessage::BranchSummary { summary, .. } => converted.push(json!({
                "role": "user",
                "content": format!("The following is a summary of a branch that this conversation came back from:\n\n<summary>\n{}\n</summary>", summary),
            })),
        }
    }
    converted
}

fn parse_chat_completion(value: Value) -> Result<CompletionResponse> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let mut assistant_blocks = Vec::new();
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            assistant_blocks.push(AssistantContentBlock::Text(TextContent::new(content)));
        }
    }

    let mut tool_calls = Vec::new();
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("call")
            .to_string();
        let function = call.get("function").cloned().unwrap_or_else(|| json!({}));
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let arguments_str = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let arguments = serde_json::from_str(arguments_str).unwrap_or_else(|_| json!({}));
        assistant_blocks.push(AssistantContentBlock::ToolCall(ToolCallContent::new(
            id.clone(),
            name.clone(),
            arguments.clone(),
        )));
        tool_calls.push(CompletionToolCall {
            id,
            name,
            arguments,
        });
    }

    let usage = value.get("usage").cloned().unwrap_or_else(|| json!({}));
    let input = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let output = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input + output);
    let stop_reason = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("length") => "max_output_tokens",
        Some("tool_calls") => "tool_calls",
        Some(other) => other,
        None => "stop",
    }
    .to_string();

    Ok(CompletionResponse {
        assistant_blocks,
        tool_calls,
        usage: Usage {
            input,
            output,
            total_tokens,
            ..Usage::default()
        },
        stop_reason,
    })
}
