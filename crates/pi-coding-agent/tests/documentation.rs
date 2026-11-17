//! Port of packages/coding-agent/test/documentation.test.ts

use std::path::{Component, Path, PathBuf};

use indexmap::{IndexMap, IndexSet};
use pi_tui::{Marked, Token};
use serde_json::{Map, Value};

#[derive(Debug)]
struct NavigationItem {
    #[allow(dead_code)]
    title: String,
    path: Option<String>,
    items: Option<Vec<NavigationItem>>,
}

#[derive(Debug)]
struct NavigationGroup {
    #[allow(dead_code)]
    title: String,
    items: Vec<NavigationItem>,
}

fn docs_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("package").join("docs")
}

fn require_object<'a>(value: &'a Value, location: &str) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{location} must be an object"))
}

fn require_nonempty_string(value: Option<&Value>, location: &str) -> String {
    let Some(s) = value.and_then(Value::as_str) else {
        panic!("{location} must be a nonempty string");
    };
    if s.trim().is_empty() {
        panic!("{location} must be a nonempty string");
    }
    s.to_owned()
}

fn require_fields(obj: &Map<String, Value>, fields: &[&str], location: &str) {
    let mut unsupported: Vec<String> = obj
        .keys()
        .filter(|field| !fields.contains(&field.as_str()))
        .cloned()
        .collect();
    unsupported.sort();
    assert!(
        unsupported.is_empty(),
        "{location} has unsupported fields: {}",
        unsupported.join(", ")
    );
}

fn normalize_posix_path(path: &str) -> String {
    let is_abs = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if !matches!(stack.last(), None | Some(&"..")) {
                    stack.pop();
                } else if !is_abs {
                    stack.push("..");
                }
            }
            other => stack.push(other),
        }
    }
    let mut out = stack.join("/");
    if is_abs {
        out = format!("/{out}");
    } else if out.is_empty() {
        out = ".".to_owned();
    }
    out
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn require_documentation_path(value: Option<&Value>, location: &str) -> String {
    let path = require_nonempty_string(value, location);
    let normalized = normalize_posix_path(&path);
    let escapes_docs_root = normalized == ".." || normalized.starts_with("../");
    let has_url_syntax = path.contains('?') || path.contains('#');
    let has_windows_syntax = path.contains('\\') || has_windows_drive_prefix(&path);

    if path != path.trim()
        || path != normalized
        || path.starts_with('/')
        || escapes_docs_root
        || has_url_syntax
        || has_windows_syntax
        || !path.ends_with(".md")
    {
        panic!("{location} must be a relative normalized .md path (got {path:?})");
    }
    path
}

fn read_navigation_item(raw_item: &Value, location: &str) -> NavigationItem {
    let item = require_object(raw_item, location);
    require_fields(item, &["title", "path", "items"], location);
    let title = require_nonempty_string(item.get("title"), &format!("{location}.title"));
    let path = item
        .get("path")
        .map(|val| require_documentation_path(Some(val), &format!("{location}.path")));

    let items = item.get("items").map(|raw_items| {
        let arr = raw_items
            .as_array()
            .unwrap_or_else(|| panic!("{location}.items must be an array"));
        assert!(!arr.is_empty(), "{location}.items must contain at least one item");
        arr.iter()
            .enumerate()
            .map(|(index, child)| read_navigation_item(child, &format!("{location}.items[{index}]")))
            .collect::<Vec<_>>()
    });

    assert!(
        path.is_some() || items.is_some(),
        "{location} must have a path or items"
    );
    NavigationItem { title, path, items }
}

fn read_navigation(docs_dir: &Path) -> Vec<NavigationGroup> {
    let raw = std::fs::read_to_string(docs_dir.join("docs.json")).expect("read docs.json");
    let parsed: Value = pi_js::json::parse(&raw).expect("parse docs.json");
    let catalog = require_object(&parsed, "Documentation catalog");
    require_fields(catalog, &["navigation", "redirects"], "Documentation catalog");
    let nav = catalog
        .get("navigation")
        .and_then(Value::as_array)
        .expect("Documentation catalog navigation must be an array");

    nav.iter()
        .enumerate()
        .map(|(group_index, raw_group)| {
            let location = format!("Documentation catalog navigation[{group_index}]");
            let group = require_object(raw_group, &location);
            require_fields(group, &["title", "items"], &location);
            let title = require_nonempty_string(group.get("title"), &format!("{location}.title"));
            let items_arr = group
                .get("items")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("{location}.items must be an array"));
            assert!(!items_arr.is_empty(), "{location}.items must contain at least one item");
            NavigationGroup {
                title,
                items: items_arr
                    .iter()
                    .enumerate()
                    .map(|(item_index, item)| read_navigation_item(item, &format!("{location}.items[{item_index}]")))
                    .collect(),
            }
        })
        .collect()
}

fn navigation_paths(groups: &[NavigationGroup]) -> Vec<String> {
    fn visit(items: &[NavigationItem], out: &mut Vec<String>) {
        for item in items {
            if let Some(ref p) = item.path {
                out.push(p.clone());
            }
            if let Some(ref children) = item.items {
                visit(children, out);
            }
        }
    }
    let mut paths = Vec::new();
    for group in groups {
        visit(&group.items, &mut paths);
    }
    paths
}

fn collect_links_from_tokens(tokens: &[Token], links: &mut Vec<String>) {
    for token in tokens {
        match token {
            Token::Link(l) => {
                links.push(l.href.clone());
                collect_links_from_tokens(&l.tokens, links);
            }
            Token::Blockquote(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Del(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Em(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Heading(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Image(t) => collect_links_from_tokens(&t.tokens, links),
            Token::List(t) => {
                for item in &t.items {
                    collect_links_from_tokens(&item.tokens, links);
                }
            }
            Token::ListItem(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Paragraph(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Strong(t) => collect_links_from_tokens(&t.tokens, links),
            Token::Table(t) => {
                for cell in &t.header {
                    collect_links_from_tokens(&cell.tokens, links);
                }
                for row in &t.rows {
                    for cell in row {
                        collect_links_from_tokens(&cell.tokens, links);
                    }
                }
            }
            Token::Text(t) => {
                if let Some(ref inner) = t.tokens {
                    collect_links_from_tokens(inner, links);
                }
            }
            Token::Generic(t) => {
                if let Some(ref inner) = t.tokens {
                    collect_links_from_tokens(inner, links);
                }
            }
            Token::Br(_)
            | Token::Checkbox(_)
            | Token::Code(_)
            | Token::Codespan(_)
            | Token::Def(_)
            | Token::Escape(_)
            | Token::Hr(_)
            | Token::Html(_)
            | Token::Space(_)
            | Token::Tag(_) => {}
        }
    }
}

fn markdown_links(markdown: &str) -> Vec<String> {
    let marked = Marked::new();
    let tokens = marked.lexer(markdown, None);
    let mut links = Vec::new();
    collect_links_from_tokens(&tokens.tokens, &mut links);
    links
}

fn collect_md_files(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).expect("read docs dir");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let rel = path.strip_prefix(root).expect("strip docs root");
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn public_slug(path: &str) -> String {
    let without_ext = &path[..path.len() - 3];
    if without_ext == "index" {
        return String::new();
    }
    if let Some(prefix) = without_ext.strip_suffix("/index") {
        prefix.to_owned()
    } else {
        without_ext.to_owned()
    }
}

fn has_uri_scheme(href: &str) -> bool {
    let Some(colon) = href.find(':') else {
        return false;
    };
    let scheme = &href[..colon];
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn lexical_join_relative(docs_dir: &Path, base_rel_file: &str, linked_path: &str) -> Option<String> {
    let parent = Path::new(base_rel_file).parent().unwrap_or(Path::new(""));
    let mut stack: Vec<String> = parent
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    for seg in linked_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => stack.push(other.to_owned()),
        }
    }
    let rel = stack.join("/");
    if docs_dir.join(&rel).is_file() { Some(rel) } else { None }
}

#[test]
fn has_valid_navigation_and_no_orphaned_markdown_pages() {
    let docs_dir = docs_root();
    let mut all_pages = Vec::new();
    collect_md_files(&docs_dir, &docs_dir, &mut all_pages);
    all_pages.sort();

    let roots = navigation_paths(&read_navigation(&docs_dir));

    let mut seen_roots = IndexSet::new();
    let mut dup_roots = IndexSet::new();
    for path in &roots {
        if !seen_roots.insert(path.clone()) {
            dup_roots.insert(path.clone());
        }
    }
    let mut duplicate_roots: Vec<String> = dup_roots.into_iter().collect();
    duplicate_roots.sort();

    let mut missing_roots: Vec<String> = roots
        .iter()
        .filter(|path| !docs_dir.join(path).is_file())
        .cloned()
        .collect();
    missing_roots.sort();

    let mut paths_by_slug: IndexMap<String, Vec<String>> = IndexMap::new();
    for path in &all_pages {
        paths_by_slug.entry(public_slug(path)).or_default().push(path.clone());
    }
    let mut slug_collisions: Vec<String> = paths_by_slug
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(slug, paths)| {
            let label = if slug.is_empty() { "/" } else { &slug };
            format!("{label} ({})", paths.join(", "))
        })
        .collect();
    slug_collisions.sort();

    let mut reachable = IndexSet::new();
    let mut pending: std::collections::VecDeque<String> = roots
        .iter()
        .filter(|path| docs_dir.join(path).is_file())
        .cloned()
        .collect();
    let mut broken_links = Vec::new();

    while let Some(source) = pending.pop_front() {
        if !reachable.insert(source.clone()) {
            continue;
        }
        let content = std::fs::read_to_string(docs_dir.join(&source)).expect("read doc markdown");
        for href in markdown_links(&content) {
            if has_uri_scheme(&href) || href.starts_with("//") || href.starts_with('/') {
                continue;
            }
            let linked_path = href.split(['?', '#']).next().unwrap_or("");
            if !linked_path.ends_with(".md") {
                continue;
            }
            if linked_path.starts_with('/') {
                continue;
            }
            let normalized_linked = linked_path.replace('\\', "/");
            match lexical_join_relative(&docs_dir, &source, &normalized_linked) {
                Some(rel_target) => {
                    if !reachable.contains(&rel_target) {
                        pending.push_back(rel_target);
                    }
                }
                None => {
                    broken_links.push(format!("{source} -> {href}"));
                }
            }
        }
    }

    let orphaned_pages: Vec<String> = all_pages
        .iter()
        .filter(|path| !reachable.contains(*path))
        .cloned()
        .collect();

    let mut issues = Vec::new();
    if !duplicate_roots.is_empty() {
        issues.push(format!("Duplicate navigation paths: {}", duplicate_roots.join(", ")));
    }
    if !missing_roots.is_empty() {
        issues.push(format!("Missing navigation pages: {}", missing_roots.join(", ")));
    }
    if !broken_links.is_empty() {
        broken_links.sort();
        issues.push(format!("Broken local Markdown links: {}", broken_links.join(", ")));
    }
    if !slug_collisions.is_empty() {
        issues.push(format!(
            "Duplicate public documentation slugs: {}",
            slug_collisions.join("; ")
        ));
    }
    if !orphaned_pages.is_empty() {
        issues.push(format!("Orphaned Markdown pages: {}", orphaned_pages.join(", ")));
    }

    pretty_assertions::assert_eq!(issues, Vec::<String>::new());
}
