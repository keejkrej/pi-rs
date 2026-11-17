//! Git diff parsing, workspace file scanning/reading, and async shell execution for `GuiBridge`.

use std::path::{Path, PathBuf};

use indexmap::{IndexMap, IndexSet};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use super::{BridgeEmitter, BridgeEvent};
use crate::models::infer_language_from_path;
use crate::state::{
    DiffFileViewModel, DiffHunkViewModel, DiffLineKind, DiffLineViewModel, DiffSummary, FileTreeEntryView,
    OpenFileView, TerminalOutputLine,
};

async fn run_git_capture(cwd: &str, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

pub(crate) async fn collect_git_diff(cwd: &str) -> (DiffSummary, IndexSet<String>) {
    let branch = run_git_capture(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let status = run_git_capture(cwd, &["status", "--porcelain=v1"])
        .await
        .unwrap_or_default();
    let diff = run_git_capture(cwd, &["diff", "HEAD", "--unified=3"])
        .await
        .unwrap_or_default();
    let summary = parse_git_diff(branch, &status, &diff);
    let modified = summary.files.iter().map(|f| f.path.clone()).collect();
    (summary, modified)
}

fn push_diff_line(
    hunk: &mut DiffHunkViewModel,
    kind: DiffLineKind,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
) {
    hunk.lines.push(DiffLineViewModel {
        kind,
        old_line,
        new_line,
        text: text.to_string(),
    });
}

fn empty_diff_file(path: String, status: String) -> DiffFileViewModel {
    DiffFileViewModel {
        path,
        status,
        additions: 0,
        deletions: 0,
        hunks: Vec::new(),
        collapsed: false,
    }
}

/// Parses `git status --porcelain=v1` and unified `git diff` output into a [`DiffSummary`].
#[must_use]
pub fn parse_git_diff(branch: Option<String>, status_porcelain: &str, diff_output: &str) -> DiffSummary {
    let mut statuses: IndexMap<String, String> = IndexMap::new();
    for line in status_porcelain.lines().filter(|l| l.len() >= 4) {
        let raw = line[3..].trim();
        let path = raw
            .rsplit_once(" -> ")
            .map_or(raw, |(_, n)| n)
            .trim_matches('"')
            .to_string();
        let label = match &line[..2] {
            "??" => "untracked",
            s if s.contains('A') => "added",
            s if s.contains('D') => "deleted",
            s if s.contains('R') => "renamed",
            _ => "modified",
        };
        statuses.insert(path, label.to_string());
    }
    let mut files: Vec<DiffFileViewModel> = Vec::new();
    let (mut cur_file, mut cur_hunk): (Option<DiffFileViewModel>, Option<DiffHunkViewModel>) = (None, None);
    let (mut old_ln, mut new_ln) = (0usize, 0usize);
    let push_hunk = |f: &mut Option<DiffFileViewModel>, h: &mut Option<DiffHunkViewModel>| {
        if let (Some(file), Some(hunk)) = (f.as_mut(), h.take()) {
            file.hunks.push(hunk);
        }
    };
    for line in diff_output.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            push_hunk(&mut cur_file, &mut cur_hunk);
            if let Some(done) = cur_file.take() {
                files.push(done);
            }
            let path = rest.split(" b/").nth(1).unwrap_or(rest).trim_matches('"').to_string();
            let status = statuses.get(&path).cloned().unwrap_or_else(|| "modified".into());
            cur_file = Some(empty_diff_file(path, status));
            continue;
        }
        let Some(file) = cur_file.as_mut() else {
            continue;
        };
        if line.starts_with("new file mode ") {
            file.status = "added".into();
        } else if line.starts_with("deleted file mode ") {
            file.status = "deleted".into();
        } else if line.starts_with("@@ ") {
            push_hunk(&mut cur_file, &mut cur_hunk);
            (old_ln, new_ln) = parse_hunk_starts(line);
            cur_hunk = Some(DiffHunkViewModel {
                header: line.into(),
                lines: Vec::new(),
            });
        } else if let Some(hunk) = cur_hunk.as_mut() {
            if line.starts_with('+') && !line.starts_with("+++") {
                file.additions += 1;
                push_diff_line(hunk, DiffLineKind::Added, None, Some(new_ln), line);
                new_ln += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                file.deletions += 1;
                push_diff_line(hunk, DiffLineKind::Removed, Some(old_ln), None, line);
                old_ln += 1;
            } else if !line.starts_with('\\') {
                push_diff_line(hunk, DiffLineKind::Context, Some(old_ln), Some(new_ln), line);
                old_ln += 1;
                new_ln += 1;
            }
        }
    }
    push_hunk(&mut cur_file, &mut cur_hunk);
    if let Some(done) = cur_file.take() {
        files.push(done);
    }
    let seen: IndexSet<String> = files.iter().map(|f| f.path.clone()).collect();
    for (path, status) in statuses {
        if !seen.contains(&path) {
            files.push(empty_diff_file(path, status));
        }
    }
    DiffSummary {
        branch,
        total_additions: files.iter().map(|f| f.additions).sum(),
        total_deletions: files.iter().map(|f| f.deletions).sum(),
        selected_path: files.first().map(|f| f.path.clone()),
        files,
    }
}

fn parse_hunk_starts(header: &str) -> (usize, usize) {
    let side = |t: &str| {
        t.trim_start_matches(['-', '+'])
            .split(',')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    };
    let mut parts = header.split_whitespace();
    let _ = parts.next();
    (parts.next().map_or(1, side), parts.next().map_or(1, side))
}

pub(crate) fn scan_workspace_files(cwd: &str, modified: &IndexSet<String>) -> Vec<FileTreeEntryView> {
    let (root, mut out) = (Path::new(cwd), Vec::new());
    walk_workspace_dir(root, root, 0, modified, &mut out);
    out
}

fn walk_workspace_dir(
    root: &Path,
    dir: &Path,
    depth: usize,
    modified: &IndexSet<String>,
    out: &mut Vec<FileTreeEntryView>,
) {
    if depth > 6 || out.len() >= 500 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(String, PathBuf, bool)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".DS_Store") {
                return None;
            }
            Some((name, e.path(), e.file_type().ok().is_some_and(|ft| ft.is_dir())))
        })
        .collect();
    entries.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    for (name, path, is_dir) in entries {
        if out.len() >= 500 {
            break;
        }
        let rel = path
            .strip_prefix(root)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or(&name)
            .to_string();
        let is_mod = modified.contains(&rel);
        out.push(FileTreeEntryView {
            path: rel,
            name,
            is_dir,
            depth,
            expanded: depth < 1,
            modified: is_mod,
        });
        if is_dir {
            walk_workspace_dir(root, &path, depth + 1, modified, out);
        }
    }
}

pub(crate) fn read_workspace_file(cwd: &str, requested: &str) -> Result<OpenFileView, String> {
    let root = std::fs::canonicalize(cwd).map_err(|e| format!("Invalid workspace cwd: {e}"))?;
    let candidate = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        root.join(requested)
    };
    let canonical = std::fs::canonicalize(&candidate).map_err(|e| format!("Failed to resolve {requested}: {e}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("Access outside workspace denied: {requested}"));
    }
    let bytes = std::fs::read(&canonical).map_err(|e| format!("Failed to read {requested}: {e}"))?;
    let content = if bytes.len() > 262_144 {
        format!(
            "{}\n\n... [truncated after 256 KiB]",
            String::from_utf8_lossy(&bytes[..262_144])
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let rel = canonical
        .strip_prefix(&root)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or(requested)
        .to_string();
    Ok(OpenFileView {
        language: infer_language_from_path(&rel).map(ToOwned::to_owned),
        path: rel,
        line_count: content.lines().count().max(1),
        content,
        dirty: false,
    })
}

fn pipe_output<R: AsyncRead + Unpin + 'static>(
    stream: Option<R>,
    is_stderr: bool,
    emitter: &BridgeEmitter,
) -> Option<tokio::task::JoinHandle<()>> {
    stream.map(|s| {
        let em = emitter.clone();
        tokio::task::spawn_local(async move {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(text)) = lines.next_line().await {
                em.emit(BridgeEvent::TerminalOutput(TerminalOutputLine { text, is_stderr }));
            }
        })
    })
}

pub(crate) fn spawn_shell_command(cwd: String, command: String, emitter: BridgeEmitter) {
    tokio::task::spawn_local(async move {
        emitter.emit(BridgeEvent::TerminalOutput(TerminalOutputLine {
            text: format!("$ {command}"),
            is_stderr: false,
        }));
        let spawned = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(err) => {
                emitter.emit(BridgeEvent::TerminalOutput(TerminalOutputLine {
                    text: format!("spawn error: {err}"),
                    is_stderr: true,
                }));
                emitter.emit(BridgeEvent::TerminalFinished { exit_code: None });
                return;
            }
        };
        let out_task = pipe_output(child.stdout.take(), false, &emitter);
        let err_task = pipe_output(child.stderr.take(), true, &emitter);
        let status = child.wait().await.ok();
        if let Some(t) = out_task {
            let _ = t.await;
        }
        if let Some(t) = err_task {
            let _ = t.await;
        }
        emitter.emit(BridgeEvent::TerminalFinished {
            exit_code: status.and_then(|s| s.code()),
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_diff_hunks_and_statuses() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n@@ -2,3 +2,4 @@\n ctx\n-old\n+new\n+two\n";
        let s = parse_git_diff(Some("main".into()), " M src/lib.rs\n?? src/new.rs\n", diff);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.files.len(), 2);
        assert_eq!((s.total_additions, s.total_deletions), (2, 1));
        assert_eq!(s.files[1].status, "untracked");
    }

    #[test]
    fn read_workspace_file_blocks_parent_traversal() {
        let tmp = std::env::temp_dir().join("pi-gui-bridge-test");
        let _ = std::fs::create_dir_all(&tmp);
        let err = read_workspace_file(tmp.to_str().unwrap(), "../etc/passwd").unwrap_err();
        assert!(err.contains("outside") || err.contains("Failed"));
    }
}
