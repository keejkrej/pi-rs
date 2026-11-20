//! Port of packages/coding-agent/test/edit-diff-window.test.ts

use pi_coding_agent::core::tools::edit_diff::{
    TextReplacement, create_edit_patch, generate_diff_string, generate_edit_diff,
};
use pi_js::vendor::jsdiff::apply_patch;

struct Mulberry32 {
    state: u32,
}

impl Mulberry32 {
    fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    fn next(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let mut t = (self.state ^ (self.state >> 15)).wrapping_mul(1 | self.state);
        t = (t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t))) ^ t;
        let bits = t ^ (t >> 14);
        (((bits as f64) / 4_294_967_296.0) * (max as f64)).floor() as usize
    }
}

fn apply_replacements(content: &str, replacements: &[TextReplacement]) -> String {
    let mut result = String::new();
    let mut position = 0usize;
    for replacement in replacements {
        result.push_str(&content[position..replacement.match_index]);
        result.push_str(&replacement.new_text);
        position = replacement.match_index + replacement.match_length;
    }
    result.push_str(&content[position..]);
    result
}

fn random_case(rng: &mut Mulberry32, distinct: bool) -> (String, Vec<TextReplacement>, String) {
    let line_count = rng.next(400);
    let mut lines = Vec::with_capacity(line_count);
    for i in 0..line_count {
        if distinct {
            lines.push(format!("line {i}"));
        } else {
            let pick = match rng.next(4) {
                0 => "a",
                1 => "b",
                2 => "",
                _ => "}",
            };
            lines.push(pick.to_owned());
        }
    }
    let mut content = lines.join("\n");
    if rng.next(2) == 0 {
        content.push('\n');
    }

    let mut replacements = Vec::new();
    let mut position = 0usize;
    let count = 1 + rng.next(4);
    for i in 0..count {
        if position >= content.len() {
            break;
        }
        let span = (content.len() - position).min(300);
        let match_index = position + rng.next(span);
        if match_index >= content.len() {
            break;
        }
        let match_length = 1 + rng.next((content.len() - match_index).min(120));
        let new_line_count = rng.next(4);
        let new_text = if rng.next(3) == 0 {
            String::new()
        } else {
            let new_lines: Vec<String> = (0..new_line_count).map(|j| format!("new {i} {j}")).collect();
            let mut s = new_lines.join("\n");
            if rng.next(2) == 0 {
                s.push('\n');
            }
            s
        };
        replacements.push(TextReplacement {
            match_index,
            match_length,
            new_text,
        });
        position = match_index + match_length;
    }

    let new_content = apply_replacements(&content, &replacements);
    (content, replacements, new_content)
}

#[test]
fn matches_whole_file_diff_and_patch_when_lines_are_distinct() {
    let mut rng = Mulberry32::new(1);
    for run in 0..2000 {
        let (content, replacements, new_content) = random_case(&mut rng, true);
        if content == new_content {
            continue;
        }
        let windowed = generate_edit_diff("f.txt", &content, &new_content, &replacements, None);
        let whole = generate_diff_string(&content, &new_content, None);
        let whole_patch = create_edit_patch("f.txt", &content, &new_content);
        assert_eq!(windowed.diff, whole.diff, "run {run} diff");
        assert_eq!(windowed.patch, whole_patch, "run {run} patch");
        assert_eq!(
            windowed.first_changed_line, whole.first_changed_line,
            "run {run} first_changed_line"
        );
    }
}

#[test]
fn produces_patch_that_applies_when_lines_repeat() {
    let mut rng = Mulberry32::new(2);
    for run in 0..2000 {
        let (content, replacements, new_content) = random_case(&mut rng, false);
        if content == new_content {
            continue;
        }
        let windowed = generate_edit_diff("f.txt", &content, &new_content, &replacements, None);
        let patched = apply_patch(&content, windowed.patch.as_str(), None).unwrap();
        assert_eq!(patched.as_deref(), Some(new_content.as_str()), "run {run}");
    }
}

#[test]
fn handles_replacement_of_many_lines() {
    let new_text = "x\n".repeat(150_000);
    let replacements = [TextReplacement {
        match_index: 0,
        match_length: 2,
        new_text: new_text.clone(),
    }];
    let expected = format!("{new_text}b\n");
    let windowed = generate_edit_diff("f.txt", "a\nb\n", &expected, &replacements, None);
    let patched = apply_patch("a\nb\n", windowed.patch.as_str(), None).unwrap();
    assert_eq!(patched.as_deref(), Some(expected.as_str()));
}

#[test]
fn diffs_small_change_in_20mb_file_via_windowing() {
    let filler = format!("{}\n", "x".repeat(99));
    let half = filler.repeat(100_000);
    let base = format!("{half}TARGET = 1\n{half}");
    let changed = format!("{half}TARGET = 2\n{half}");
    let replacements = [TextReplacement {
        match_index: half.len(),
        match_length: 10,
        new_text: "TARGET = 2".to_owned(),
    }];
    let windowed = generate_edit_diff("big.txt", &base, &changed, &replacements, None);
    let changed_lines: Vec<&str> = windowed
        .diff
        .split('\n')
        .filter(|line| line.starts_with('+') || line.starts_with('-'))
        .collect();
    assert_eq!(changed_lines.join("\n"), "-100001 TARGET = 1\n+100001 TARGET = 2");
}
