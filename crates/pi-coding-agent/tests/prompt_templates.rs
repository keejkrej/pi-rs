//! Port of `packages/coding-agent/test/prompt-templates.test.ts`.

use std::path::Path;

use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::diagnostics::ResourceDiagnosticType;
use pi_coding_agent::core::prompt_templates::{
    LoadPromptTemplatesOptions, PromptTemplate, expand_prompt_template, load_prompt_templates, parse_command_args,
    substitute_args,
};
use pi_coding_agent::core::source_info::{SyntheticSourceOptions, create_synthetic_source_info};
use tempfile::TempDir;

fn synthetic_template(name: &str, content: &str) -> PromptTemplate {
    let path = format!("/tmp/{name}.md");
    PromptTemplate {
        name: name.to_owned(),
        description: "test".to_owned(),
        argument_hint: None,
        content: content.to_owned(),
        source_info: create_synthetic_source_info(
            &path,
            SyntheticSourceOptions {
                source: "local".to_owned(),
                scope: None,
                origin: None,
                base_dir: None,
                changelog_path: None,
            },
        ),
        file_path: path,
    }
}

#[test]
fn substitute_args_basic_and_non_recursive() {
    let args = |s: &[&str]| -> Vec<String> { s.iter().map(|x| (*x).to_owned()).collect() };

    assert_eq!(
        substitute_args("Test: $ARGUMENTS", &args(&["a", "b", "c"])),
        "Test: a b c"
    );
    assert_eq!(substitute_args("Test: $@", &args(&["a", "b", "c"])), "Test: a b c");
    assert_eq!(
        substitute_args("Test: $@", &args(&["foo", "bar", "baz"])),
        substitute_args("Test: $ARGUMENTS", &args(&["foo", "bar", "baz"]))
    );

    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["$1", "$ARGUMENTS"])),
        "$1 $ARGUMENTS"
    );
    assert_eq!(substitute_args("$@", &args(&["$100", "$1"])), "$100 $1");
    assert_eq!(substitute_args("$ARGUMENTS", &args(&["$100", "$1"])), "$100 $1");

    assert_eq!(
        substitute_args("$1: $ARGUMENTS", &args(&["prefix", "a", "b"])),
        "prefix: prefix a b"
    );
    assert_eq!(
        substitute_args("$1: $@", &args(&["prefix", "a", "b"])),
        "prefix: prefix a b"
    );
    assert_eq!(substitute_args("Test: $ARGUMENTS", &args(&[])), "Test: ");
    assert_eq!(substitute_args("Test: $@", &args(&[])), "Test: ");
    assert_eq!(substitute_args("Test: $1", &args(&[])), "Test: ");

    assert_eq!(
        substitute_args("$ARGUMENTS and $ARGUMENTS", &args(&["a", "b"])),
        "a b and a b"
    );
    assert_eq!(substitute_args("$@ and $@", &args(&["a", "b"])), "a b and a b");
    assert_eq!(substitute_args("$@ and $ARGUMENTS", &args(&["a", "b"])), "a b and a b");
    assert_eq!(
        substitute_args("$1 $2: $ARGUMENTS", &args(&["arg100", "@user"])),
        "arg100 @user: arg100 @user"
    );
    assert_eq!(substitute_args("$1 $2 $3 $4 $5", &args(&["a", "b"])), "a b   ");
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["日本語", "🎉", "café"])),
        "日本語 🎉 café"
    );
    assert_eq!(
        substitute_args("$1 $2", &args(&["line1\nline2", "tab\tthere"])),
        "line1\nline2 tab\tthere"
    );
    assert_eq!(substitute_args("$1$2", &args(&["a", "b"])), "ab");
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["first arg", "second arg"])),
        "first arg second arg"
    );
    assert_eq!(substitute_args("Test: $ARGUMENTS", &args(&["only"])), "Test: only");
    assert_eq!(substitute_args("Test: $@", &args(&["only"])), "Test: only");
    assert_eq!(substitute_args("$0", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("$1.5", &args(&["a"])), "a.5");
    assert_eq!(substitute_args("pre$ARGUMENTS", &args(&["a", "b"])), "prea b");
    assert_eq!(substitute_args("pre$@", &args(&["a", "b"])), "prea b");
    assert_eq!(substitute_args("$ARGUMENTS", &args(&["a", "", "c"])), "a  c");
    assert_eq!(
        substitute_args("$ARGUMENTS", &args(&["  leading  ", "trailing  "])),
        "  leading   trailing  "
    );
    assert_eq!(
        substitute_args("Prefix $ARGUMENTS suffix", &args(&["ARGUMENTS"])),
        "Prefix ARGUMENTS suffix"
    );
    assert_eq!(substitute_args("$A $$ $ $ARGS", &args(&["a"])), "$A $$ $ $ARGS");
    assert_eq!(
        substitute_args("$arguments $Arguments $ARGUMENTS", &args(&["a", "b"])),
        "$arguments $Arguments a b"
    );

    let many: Vec<String> = (0..100).map(|i| format!("arg{i}")).collect();
    assert_eq!(substitute_args("$ARGUMENTS", &many), many.join(" "));
    let vals: Vec<String> = (0..15).map(|i| format!("val{i}")).collect();
    assert_eq!(substitute_args("$10 $12 $15", &vals), "val9 val11 val14");
    assert_eq!(substitute_args("Price: \\$100", &args(&[])), "Price: \\");
    assert_eq!(
        substitute_args("$1: $@ ($ARGUMENTS)", &args(&["first", "second", "third"])),
        "first: first second third (first second third)"
    );
    assert_eq!(
        substitute_args("Just plain text", &args(&["a", "b"])),
        "Just plain text"
    );
    assert_eq!(substitute_args("$1 $2 $@", &args(&["a", "b", "c"])), "a b a b c");
}

#[test]
fn substitute_args_positional_defaults_and_slicing() {
    let args = |s: &[&str]| -> Vec<String> { s.iter().map(|x| (*x).to_owned()).collect() };

    assert_eq!(
        substitute_args("List exactly ${1:-7} next steps", &args(&[])),
        "List exactly 7 next steps"
    );
    let tpl = "${@:-default}\n${ARGUMENTS:-default}";
    assert_eq!(substitute_args(tpl, &args(&[])), "default\ndefault");
    assert_eq!(
        substitute_args(tpl, &args(&["This", "would", "be", "the", "arguments"])),
        "This would be the arguments\nThis would be the arguments"
    );
    assert_eq!(
        substitute_args("List exactly ${1:-7} next steps", &args(&["3"])),
        "List exactly 3 next steps"
    );
    assert_eq!(substitute_args("Mode: ${1:-brief}", &args(&[""])), "Mode: brief");
    assert_eq!(substitute_args("${1:-7} ${2:-brief}", &args(&[])), "7 brief");
    assert_eq!(substitute_args("${1:-7} ${2:-brief}", &args(&["3"])), "3 brief");
    assert_eq!(
        substitute_args("${1:-7} ${2:-brief}", &args(&["3", "verbose"])),
        "3 verbose"
    );
    assert_eq!(substitute_args("${1:-7}", &args(&["$ARGUMENTS"])), "$ARGUMENTS");
    assert_eq!(substitute_args("${1:-7}", &args(&["$1"])), "$1");
    assert_eq!(substitute_args("${1:-$ARGUMENTS}", &args(&["a", "b"])), "a");
    assert_eq!(substitute_args("${3:-$ARGUMENTS}", &args(&["a", "b"])), "$ARGUMENTS");
    assert_eq!(substitute_args("${1:-seven steps}", &args(&[])), "seven steps");
    assert_eq!(substitute_args("${3:-fallback}", &args(&["a", "b"])), "fallback");
    assert_eq!(substitute_args("$1 ${2:-x} $ARGUMENTS", &args(&["a"])), "a x a");

    // Array slicing
    assert_eq!(substitute_args("${@:2}", &args(&["a", "b", "c", "d"])), "b c d");
    assert_eq!(substitute_args("${@:1}", &args(&["a", "b", "c"])), "a b c");
    assert_eq!(substitute_args("${@:3}", &args(&["a", "b", "c", "d"])), "c d");
    assert_eq!(substitute_args("${@:2:2}", &args(&["a", "b", "c", "d"])), "b c");
    assert_eq!(substitute_args("${@:1:1}", &args(&["a", "b", "c"])), "a");
    assert_eq!(substitute_args("${@:3:1}", &args(&["a", "b", "c", "d"])), "c");
    assert_eq!(substitute_args("${@:2:3}", &args(&["a", "b", "c", "d", "e"])), "b c d");
    assert_eq!(substitute_args("${@:99}", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("${@:10:5}", &args(&["a", "b"])), "");
    assert_eq!(substitute_args("${@:2:0}", &args(&["a", "b", "c"])), "");
    assert_eq!(substitute_args("${@:2:99}", &args(&["a", "b", "c"])), "b c");
    assert_eq!(substitute_args("${@:2} vs $@", &args(&["a", "b", "c"])), "b c vs a b c");
    assert_eq!(
        substitute_args("First: ${@:1:1}, All: $@", &args(&["x", "y", "z"])),
        "First: x, All: x y z"
    );
    assert_eq!(substitute_args("${@:1}", &args(&["${@:2}", "test"])), "${@:2} test");
    assert_eq!(substitute_args("${@:0}", &args(&["a", "b", "c"])), "a b c");
    assert_eq!(substitute_args("${@:2}", &args(&[])), "");
    assert_eq!(substitute_args("${@:1}", &args(&["only"])), "only");
    assert_eq!(substitute_args("${@:2}", &args(&["only"])), "");
    assert_eq!(
        substitute_args("Process ${@:2} with $1", &args(&["tool", "file1", "file2"])),
        "Process file1 file2 with tool"
    );
    assert_eq!(
        substitute_args("${@:1:1} and ${@:2}", &args(&["a", "b", "c"])),
        "a and b c"
    );
    assert_eq!(
        substitute_args("prefix${@:2}suffix", &args(&["a", "b", "c"])),
        "prefixb csuffix"
    );
    let ten: Vec<String> = (1..=10).map(|i| format!("arg{i}")).collect();
    assert_eq!(substitute_args("${@:5:100}", &ten), "arg5 arg6 arg7 arg8 arg9 arg10");
}

#[test]
fn parse_command_args_and_expand_prompt_template() {
    assert_eq!(parse_command_args("a b c"), vec!["a", "b", "c"]);
    assert_eq!(parse_command_args("\"first arg\" second"), vec!["first arg", "second"]);
    assert_eq!(parse_command_args("'first arg' second"), vec!["first arg", "second"]);
    assert_eq!(
        parse_command_args("\"double\" 'single' \"double again\""),
        vec!["double", "single", "double again"]
    );
    assert!(parse_command_args("").is_empty());
    assert_eq!(parse_command_args("a  b   c"), vec!["a", "b", "c"]);
    assert_eq!(parse_command_args("a\tb\tc"), vec!["a", "b", "c"]);
    assert_eq!(parse_command_args("\"\" \" \""), vec!["", " "]);
    assert_eq!(parse_command_args("a '' c"), vec!["a", "", "c"]);
    assert_eq!(parse_command_args("a \"\""), vec!["a", ""]);
    assert_eq!(parse_command_args("$100 @user #tag"), vec!["$100", "@user", "#tag"]);
    assert_eq!(parse_command_args("日本語 🎉 café"), vec!["日本語", "🎉", "café"]);
    assert_eq!(
        parse_command_args("\"line1\nline2\" second"),
        vec!["line1\nline2", "second"]
    );
    assert_eq!(
        parse_command_args("label-2\n\nHere is some description #2."),
        vec!["label-2", "Here", "is", "some", "description", "#2."]
    );
    assert_eq!(parse_command_args("a\n\n\tb  c"), vec!["a", "b", "c"]);
    assert_eq!(parse_command_args("\"quoted \\\"text\\\"\""), vec!["quoted \"text\""]);
    assert_eq!(parse_command_args("'it\\'s here' next"), vec!["it's here", "next"]);
    assert_eq!(
        parse_command_args("why doesn't this work"),
        vec!["why", "doesn't", "this", "work"]
    );
    assert_eq!(parse_command_args("a b c   "), vec!["a", "b", "c"]);
    assert_eq!(parse_command_args("   a b c"), vec!["a", "b", "c"]);

    let expanded = expand_prompt_template(
        "/arg-test label-2\n\nHere is some description #2.",
        &[synthetic_template("arg-test", "- arg1: $1\n- rest: ${@:2}")],
    );
    assert_eq!(expanded, "- arg1: label-2\n- rest: Here is some description #2.");

    let newline_sep = expand_prompt_template("/arg-test\nlabel-2", &[synthetic_template("arg-test", "arg1: $1")]);
    assert_eq!(newline_sep, "arg1: label-2");

    let args = parse_command_args("Button \"onClick handler\" \"disabled support\"");
    assert_eq!(
        substitute_args("Create component $1 with features: $ARGUMENTS", &args),
        "Create component Button with features: Button onClick handler disabled support"
    );
}

#[test]
fn load_prompt_templates_argument_hint_and_invalid_yaml_diagnostics() {
    let tmp = TempDir::new().unwrap();
    let write_tpl = |dir: &Path, name: &str, content: &str| {
        std::fs::write(dir.join(format!("{name}.md")), content).unwrap();
    };

    write_tpl(
        tmp.path(),
        "pr",
        "---\ndescription: Review PRs from URLs with structured issue and code analysis\nargument-hint: \"<PR-URL>\"\n---\nYou are given one or more GitHub PR URLs: $@",
    );
    write_tpl(
        tmp.path(),
        "wr",
        "---\ndescription: Finish the current task end-to-end with changelog, commit, and push\nargument-hint: \"[instructions]\"\n---\nWrap it. Additional instructions: $ARGUMENTS",
    );
    write_tpl(
        tmp.path(),
        "cl",
        "---\ndescription: Audit changelog entries before release\n---\nAudit changelog entries for all commits since the last release.",
    );
    write_tpl(
        tmp.path(),
        "empty-hint",
        "---\ndescription: A command with empty hint\nargument-hint: \"\"\n---\nDo something",
    );
    write_tpl(
        tmp.path(),
        "is",
        "---\ndescription: Analyze GitHub issues (bugs or feature requests)\nargument-hint: \"<issue>\"\n---\nAnalyze GitHub issue(s): $ARGUMENTS",
    );

    let res = load_prompt_templates(LoadPromptTemplatesOptions {
        cwd: pi_js::env::cwd(),
        agent_dir: get_agent_dir(),
        prompt_paths: vec![tmp.path().to_str().unwrap().to_owned()],
        include_defaults: false,
    });

    let find = |name: &str| res.templates.iter().find(|t| t.name == name).unwrap();
    assert_eq!(find("pr").argument_hint.as_deref(), Some("<PR-URL>"));
    assert_eq!(find("wr").argument_hint.as_deref(), Some("[instructions]"));
    assert_eq!(find("cl").argument_hint, None);
    assert_eq!(find("empty-hint").argument_hint, None);
    assert_eq!(find("is").argument_hint.as_deref(), Some("<issue>"));

    // #9354 invalid YAML frontmatter warning diagnostic
    let invalid_dir = TempDir::new().unwrap();
    let invalid_path = invalid_dir.path().join("invalid.md");
    std::fs::write(
        &invalid_path,
        "---\ndescription: Broken: unquoted colon\n---\nDo something.\n",
    )
    .unwrap();
    std::fs::write(invalid_dir.path().join("valid.md"), "Valid prompt content.").unwrap();

    let invalid_res = load_prompt_templates(LoadPromptTemplatesOptions {
        cwd: pi_js::env::cwd(),
        agent_dir: get_agent_dir(),
        prompt_paths: vec![invalid_dir.path().to_str().unwrap().to_owned()],
        include_defaults: false,
    });
    let names: Vec<&str> = invalid_res.templates.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["valid"]);
    assert_eq!(invalid_res.diagnostics.len(), 1);
    assert_eq!(invalid_res.diagnostics[0].r#type, ResourceDiagnosticType::Warning);
    assert_eq!(invalid_res.diagnostics[0].path.as_deref(), invalid_path.to_str());
    assert!(invalid_res.diagnostics[0].message.contains("line 1"));
}
