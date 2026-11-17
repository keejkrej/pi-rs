//! Port of packages/coding-agent/examples/extensions/github-issue-autocomplete.ts

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    ExecOptions, Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension, NotifyType,
    SessionStartEvent,
};
use pi_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, GetSuggestionsOptions,
};
use pi_tui::fuzzy::fuzzy_filter;
use serde::Deserialize;
use tokio::sync::OnceCell;

const MAX_ISSUES: usize = 100;
const MAX_SUGGESTIONS: usize = 20;

static ISSUE_TOKEN_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"(?:^|[ \t])#([^\s#]*)$").ok());
static SSH_REMOTE_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"^git@github\.com:([^/]+/[^/]+?)(?:\.git)?$").ok());
static HTTPS_REMOTE_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"^https?://github\.com/([^/]+/[^/]+?)(?:\.git)?$").ok());

#[derive(Clone, Debug, Deserialize)]
struct GitHubIssue {
    number: u64,
    title: String,
    state: String,
}

fn extract_issue_token(text_before_cursor: &str) -> Option<String> {
    let re = ISSUE_TOKEN_RE.as_ref()?;
    let caps = re.captures(text_before_cursor)?;
    Some(caps.get(1)?.as_str().to_string())
}

fn parse_github_repo(remote_url: &str) -> Option<String> {
    if let Some(re) = SSH_REMOTE_RE.as_ref()
        && let Some(caps) = re.captures(remote_url)
    {
        return caps.get(1).map(|m| m.as_str().to_string());
    }
    if let Some(re) = HTTPS_REMOTE_RE.as_ref()
        && let Some(caps) = re.captures(remote_url)
    {
        return caps.get(1).map(|m| m.as_str().to_string());
    }
    None
}

async fn resolve_github_repo(pi: &ExtensionApi, cwd: &str) -> Result<String, String> {
    let result = pi
        .exec(
            "git",
            &["remote".to_string(), "-v".to_string()],
            Some(ExecOptions {
                signal: None,
                timeout: Some(5_000),
                cwd: Some(cwd.to_string()),
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    if result.code != 0 {
        return Err("github-issue-autocomplete: cwd is not a git repository".to_string());
    }
    for line in result.stdout.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(remote_url) = cols.get(1)
            && let Some(repo) = parse_github_repo(remote_url)
        {
            return Ok(repo);
        }
    }
    Err("github-issue-autocomplete: cwd is not a GitHub repository".to_string())
}

fn format_issue_item(issue: &GitHubIssue) -> AutocompleteItem {
    AutocompleteItem {
        value: format!("#{}", issue.number),
        label: format!("#{}", issue.number),
        description: Some(format!("[{}] {}", issue.state.to_lowercase(), issue.title)),
    }
}

fn filter_issues(issues: &[GitHubIssue], query: &str) -> Vec<AutocompleteItem> {
    if query.trim().is_empty() {
        return issues.iter().take(MAX_SUGGESTIONS).map(format_issue_item).collect();
    }
    if query.chars().all(|c| c.is_ascii_digit()) {
        let numeric: Vec<AutocompleteItem> = issues
            .iter()
            .filter(|issue| issue.number.to_string().starts_with(query))
            .take(MAX_SUGGESTIONS)
            .map(format_issue_item)
            .collect();
        if !numeric.is_empty() {
            return numeric;
        }
    }
    fuzzy_filter(
        issues,
        query,
        Arc::new(|issue: &GitHubIssue| format!("{} {}", issue.number, issue.title)),
    )
    .into_iter()
    .take(MAX_SUGGESTIONS)
    .map(|i| format_issue_item(&i))
    .collect()
}

struct IssueLoader {
    pi: ExtensionApi,
    cwd: String,
    repo: String,
    ctx: ExtensionContext,
    cache: OnceCell<Option<Vec<GitHubIssue>>>,
    load_error_shown: AtomicBool,
}

impl IssueLoader {
    async fn get_issues(&self) -> Option<&Vec<GitHubIssue>> {
        self.cache
            .get_or_init(|| async {
                let args = vec![
                    "issue".to_string(),
                    "list".to_string(),
                    "--repo".to_string(),
                    self.repo.clone(),
                    "--state".to_string(),
                    "open".to_string(),
                    "--limit".to_string(),
                    MAX_ISSUES.to_string(),
                    "--json".to_string(),
                    "number,title,state".to_string(),
                ];
                let Ok(result) = self
                    .pi
                    .exec(
                        "gh",
                        &args,
                        Some(ExecOptions {
                            signal: None,
                            timeout: Some(5_000),
                            cwd: Some(self.cwd.clone()),
                        }),
                    )
                    .await
                else {
                    return None;
                };

                if result.code != 0 {
                    if !self.load_error_shown.swap(true, Ordering::SeqCst)
                        && let Ok(ui) = self.ctx.ui()
                    {
                        let details = if result.stderr.trim().is_empty() {
                            format!("exit code {}", result.code)
                        } else {
                            result.stderr.trim().to_string()
                        };
                        ui.notify(
                            &format!("github-issue-autocomplete: failed to load issues: {details}"),
                            Some(NotifyType::Error),
                        );
                    }
                    return None;
                }

                match serde_json::from_str::<Vec<GitHubIssue>>(&result.stdout) {
                    Ok(issues) => Some(issues),
                    Err(_) => {
                        if !self.load_error_shown.swap(true, Ordering::SeqCst)
                            && let Ok(ui) = self.ctx.ui()
                        {
                            ui.notify(
                                "github-issue-autocomplete: failed to parse gh issue list output",
                                Some(NotifyType::Error),
                            );
                        }
                        None
                    }
                }
            })
            .await
            .as_ref()
    }
}

struct IssueAutocompleteProvider {
    current: Arc<dyn AutocompleteProvider>,
    loader: Arc<IssueLoader>,
}

#[async_trait]
impl AutocompleteProvider for IssueAutocompleteProvider {
    async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        options: GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions> {
        let line_idx = usize::try_from(cursor_line).ok()?;
        let col_idx = usize::try_from(cursor_col).ok().unwrap_or(0);
        let current_line = lines.get(line_idx).map(String::as_str).unwrap_or("");
        let text_before: String = current_line.chars().take(col_idx).collect();

        let Some(token) = extract_issue_token(&text_before) else {
            return self
                .current
                .get_suggestions(lines, cursor_line, cursor_col, options)
                .await;
        };

        let issues = self.loader.get_issues().await;
        if options.signal.aborted() {
            return self
                .current
                .get_suggestions(lines, cursor_line, cursor_col, options)
                .await;
        }
        let Some(issues) = issues.filter(|v| !v.is_empty()) else {
            return self
                .current
                .get_suggestions(lines, cursor_line, cursor_col, options)
                .await;
        };

        let suggestions = filter_issues(issues, &token);
        if suggestions.is_empty() {
            return self
                .current
                .get_suggestions(lines, cursor_line, cursor_col, options)
                .await;
        }

        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: format!("#{token}"),
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        self.current
            .apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: i64, cursor_col: i64) -> bool {
        self.current
            .should_trigger_file_completion(lines, cursor_line, cursor_col)
    }
}

pub struct GithubIssueAutocompleteExtension;

#[async_trait]
impl Extension for GithubIssueAutocompleteExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_ss = pi.clone();
        pi.on::<SessionStartEvent>(Arc::new(move |_event, ctx| {
            let pi = pi_ss.clone();
            Box::pin(async move {
                let cwd = ctx.cwd()?;
                let repo = match resolve_github_repo(&pi, &cwd).await {
                    Ok(r) => r,
                    Err(err) => {
                        ctx.ui()?.notify(&err, Some(NotifyType::Error));
                        return Ok(None);
                    }
                };

                let loader = Arc::new(IssueLoader {
                    pi: pi.clone(),
                    cwd,
                    repo,
                    ctx: ctx.clone(),
                    cache: OnceCell::new(),
                    load_error_shown: AtomicBool::new(false),
                });

                let loader_preload = Arc::clone(&loader);
                tokio::spawn(async move {
                    let _ = loader_preload.get_issues().await;
                });

                ctx.ui()?.add_autocomplete_provider(Arc::new(move |current| {
                    Arc::new(IssueAutocompleteProvider {
                        current,
                        loader: Arc::clone(&loader),
                    })
                }));

                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(GithubIssueAutocompleteExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
