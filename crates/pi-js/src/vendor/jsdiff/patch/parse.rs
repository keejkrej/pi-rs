//! Port of `diff/libesm/patch/parse.js` (diff@8.0.4).

use std::sync::LazyLock;

use regex::Regex;

use crate::error::{Error, Result};
use crate::vendor::jsdiff::types::{StructuredPatch, StructuredPatchHunk};
use crate::vendor::jsdiff::util::string::{js_trim, json_quote};

// PORT: JS `\s` as a regex character class.
const WS: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// `/^(---|\+\+\+|@@)\s/`
static FILE_HEADER_FOUND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^(?:---|\+\+\+|@@){WS}")).expect("static regex"));
/// `/^(?:Index:|diff(?: -r \w+)+)\s+/`
static DIFF_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^(?:Index:|diff(?: -r [A-Za-z0-9_]+)+){WS}+")).expect("static regex"));
/// `/^(Index:\s|diff\s|---\s|\+\+\+\s|===================================================================)/`
static NEXT_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?:Index:{WS}|diff{WS}|---{WS}|\+\+\+{WS}|===================================================================)"
    ))
    .expect("static regex")
});
/// `/^(---|\+\+\+)\s+/`
static FILE_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^(---|\+\+\+){WS}+")).expect("static regex"));
/// `/@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/`
static CHUNK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@").expect("static regex"));

/// Parses a patch into structured data, in the same structure returned by `structuredPatch`.
///
/// @return a JSON object representation of the a patch, suitable for use with the `applyPatch` method.
pub fn parse_patch(uni_diff: &str) -> Result<Vec<StructuredPatch>> {
    let mut parser = Parser {
        diffstr: uni_diff.split('\n').collect(),
        i: 0,
    };
    let mut list = Vec::new();
    while parser.i < parser.diffstr.len() {
        list.push(parser.parse_index()?);
    }
    Ok(list)
}

struct Parser<'a> {
    diffstr: Vec<&'a str>,
    i: usize,
}

impl Parser<'_> {
    fn parse_index(&mut self) -> Result<StructuredPatch> {
        let mut index = StructuredPatch::default();
        // Parse diff metadata
        while self.i < self.diffstr.len() {
            let line = self.diffstr[self.i];
            // File header found, end parsing diff metadata
            if FILE_HEADER_FOUND.is_match(line) {
                break;
            }
            // Try to parse the line as a diff header, like
            //     Index: README.md
            // or
            //     diff -r 9117c6561b0b -r 273ce12ad8f1 .hgignore
            // or
            //     Index: something with multiple words
            // and extract the filename (or whatever else is used as an index name)
            // from the end (i.e. 'README.md', '.hgignore', or
            // 'something with multiple words' in the examples above).
            //
            // TODO: It seems awkward that we indiscriminately trim off trailing
            //       whitespace here. Theoretically, couldn't that be meaningful -
            //       e.g. if the patch represents a diff of a file whose name ends
            //       with a space? Seems wrong to nuke it.
            //       But this behaviour has been around since v2.2.1 in 2015, so if
            //       it's going to change, it should be done cautiously and in a new
            //       major release, for backwards-compat reasons.
            //       -- ExplodingCabbage
            if let Some(header_match) = DIFF_HEADER.find(line) {
                index.index = Some(js_trim(&line[header_match.end()..]).to_string());
            }
            self.i += 1;
        }
        // Parse file headers if they are defined. Unified diff requires them, but
        // there's no technical issues to have an isolated hunk without file header
        self.parse_file_header(&mut index);
        self.parse_file_header(&mut index);
        // Parse hunks
        index.hunks = Vec::new();
        while self.i < self.diffstr.len() {
            let line = self.diffstr[self.i];
            if NEXT_FILE.is_match(line) {
                break;
            } else if line.starts_with("@@") {
                let hunk = self.parse_hunk()?;
                index.hunks.push(hunk);
            } else if !line.is_empty() {
                return Err(Error::msg(format!("Unknown line {} {}", self.i + 1, json_quote(line))));
            } else {
                self.i += 1;
            }
        }
        Ok(index)
    }

    // Parses the --- and +++ headers, if none are found, no lines
    // are consumed.
    fn parse_file_header(&mut self, index: &mut StructuredPatch) {
        let Some(line) = self.diffstr.get(self.i).copied() else {
            return;
        };
        let Some(file_header_match) = FILE_HEADER.captures(line) else {
            return;
        };
        let prefix = file_header_match.get(1).map_or("", |m| m.as_str());
        // .substring(3).trim().split('\t', 2)
        let mut data = js_trim(&line[3..]).split('\t');
        let data0 = data.next().unwrap_or("");
        let header = js_trim(data.next().unwrap_or("")).to_string();
        let mut file_name = data0.replace("\\\\", "\\");
        if file_name.starts_with('"') && file_name.ends_with('"') {
            // fileName.substr(1, fileName.length - 2)
            file_name = if file_name.len() >= 2 {
                file_name[1..file_name.len() - 1].to_string()
            } else {
                String::new()
            };
        }
        if prefix == "---" {
            index.old_file_name = Some(file_name);
            index.old_header = Some(header);
        } else {
            index.new_file_name = Some(file_name);
            index.new_header = Some(header);
        }
        self.i += 1;
    }

    // Parses a hunk
    // This assumes that we are at the start of a hunk.
    fn parse_hunk(&mut self) -> Result<StructuredPatchHunk> {
        let chunk_header_index = self.i;
        let chunk_header_line = self.diffstr[self.i];
        self.i += 1;
        // PORT: chunkHeaderLine.split(/@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/): only the first match's
        // groups are read; a non-matching line leaves every group `undefined`.
        let chunk_header = CHUNK_HEADER.captures(chunk_header_line);
        let group = |n: usize| -> Option<f64> {
            chunk_header
                .as_ref()
                .and_then(|c| c.get(n))
                .map(|m| m.as_str().parse::<f64>().unwrap_or(f64::NAN))
        };
        let mut hunk = StructuredPatchHunk {
            old_start: group(1).unwrap_or(f64::NAN),
            old_lines: group(2).unwrap_or(1.0),
            new_start: group(3).unwrap_or(f64::NAN),
            new_lines: group(4).unwrap_or(1.0),
            lines: Vec::new(),
        };
        // Unified Diff Format quirk: If the chunk size is 0,
        // the first number is one lower than one would expect.
        // https://www.artima.com/weblogs/viewpost.jsp?thread=164293
        if hunk.old_lines == 0.0 {
            hunk.old_start += 1.0;
        }
        if hunk.new_lines == 0.0 {
            hunk.new_start += 1.0;
        }
        let mut add_count = 0usize;
        let mut remove_count = 0usize;
        let len = self.diffstr.len();
        while self.i < len
            && ((remove_count as f64) < hunk.old_lines
                || (add_count as f64) < hunk.new_lines
                || self.diffstr[self.i].starts_with('\\'))
        {
            let line = self.diffstr[self.i];
            let operation = if line.is_empty() && self.i != len - 1 {
                Some(' ')
            } else {
                line.chars().next()
            };
            match operation {
                Some(op @ ('+' | '-' | ' ' | '\\')) => {
                    hunk.lines.push(line.to_string());
                    if op == '+' {
                        add_count += 1;
                    } else if op == '-' {
                        remove_count += 1;
                    } else if op == ' ' {
                        add_count += 1;
                        remove_count += 1;
                    }
                }
                _ => {
                    return Err(Error::msg(format!(
                        "Hunk at line {} contained invalid line {}",
                        chunk_header_index + 1,
                        line
                    )));
                }
            }
            self.i += 1;
        }
        // Handle the empty block count case
        if add_count == 0 && hunk.new_lines == 1.0 {
            hunk.new_lines = 0.0;
        }
        if remove_count == 0 && hunk.old_lines == 1.0 {
            hunk.old_lines = 0.0;
        }
        // Perform sanity checking
        if (add_count as f64) != hunk.new_lines {
            return Err(Error::msg(format!(
                "Added line count did not match for hunk at line {}",
                chunk_header_index + 1
            )));
        }
        if (remove_count as f64) != hunk.old_lines {
            return Err(Error::msg(format!(
                "Removed line count did not match for hunk at line {}",
                chunk_header_index + 1
            )));
        }
        Ok(hunk)
    }
}
