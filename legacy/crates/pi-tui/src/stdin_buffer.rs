const ESC: char = '\u{1b}';
const BRACKETED_PASTE_START: &str = "\u{1b}[200~";
const BRACKETED_PASTE_END: &str = "\u{1b}[201~";

pub type StdinBufferEventMap = Vec<StdinBufferEvent>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinBufferEvent {
    Data(String),
    Paste(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdinBufferOptions {
    pub timeout_ms: u64,
}

impl Default for StdinBufferOptions {
    fn default() -> Self {
        Self { timeout_ms: 10 }
    }
}

#[derive(Debug, Clone)]
pub struct StdinBuffer {
    buffer: String,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<u32>,
    pub options: StdinBufferOptions,
}

impl Default for StdinBuffer {
    fn default() -> Self {
        Self::new(StdinBufferOptions::default())
    }
}

impl StdinBuffer {
    pub fn new(options: StdinBufferOptions) -> Self {
        Self {
            buffer: String::new(),
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
            options,
        }
    }

    pub fn process(&mut self, data: &str) -> Vec<StdinBufferEvent> {
        if data.is_empty() && self.buffer.is_empty() {
            return vec![StdinBufferEvent::Data(String::new())];
        }

        self.buffer.push_str(data);
        let (sequences, remainder) = extract_complete_sequences(&self.buffer);
        self.buffer = remainder;

        let mut events = Vec::new();
        for sequence in sequences {
            if self.paste_mode {
                if let Some(end) = sequence.find(BRACKETED_PASTE_END) {
                    self.paste_buffer.push_str(&sequence[..end]);
                    events.push(StdinBufferEvent::Paste(std::mem::take(
                        &mut self.paste_buffer,
                    )));
                    self.paste_mode = false;
                    self.pending_kitty_printable_codepoint = None;
                    let after = &sequence[end + BRACKETED_PASTE_END.len()..];
                    if !after.is_empty() {
                        events.extend(self.process(after));
                    }
                } else {
                    self.paste_buffer.push_str(&sequence);
                }
                continue;
            }

            if let Some(start) = sequence.find(BRACKETED_PASTE_START) {
                let before = &sequence[..start];
                if !before.is_empty() {
                    events.push(StdinBufferEvent::Data(before.to_string()));
                }
                self.paste_mode = true;
                self.pending_kitty_printable_codepoint = None;
                let after = &sequence[start + BRACKETED_PASTE_START.len()..];
                if let Some(end) = after.find(BRACKETED_PASTE_END) {
                    events.push(StdinBufferEvent::Paste(after[..end].to_string()));
                    self.paste_mode = false;
                    self.pending_kitty_printable_codepoint = None;
                    let rest = &after[end + BRACKETED_PASTE_END.len()..];
                    if !rest.is_empty() {
                        events.extend(self.process(rest));
                    }
                } else {
                    self.paste_buffer.push_str(after);
                }
            } else if let Some(event) = self.emit_data_sequence(sequence) {
                events.push(event);
            }
        }
        events
    }

    fn emit_data_sequence(&mut self, sequence: String) -> Option<StdinBufferEvent> {
        let raw_codepoint = if sequence.chars().count() == 1 {
            sequence.chars().next().map(|ch| ch as u32)
        } else {
            None
        };
        if raw_codepoint.is_some() && raw_codepoint == self.pending_kitty_printable_codepoint {
            self.pending_kitty_printable_codepoint = None;
            return None;
        }

        self.pending_kitty_printable_codepoint =
            parse_unmodified_kitty_printable_codepoint(&sequence);
        Some(StdinBufferEvent::Data(sequence))
    }

    pub fn flush(&mut self) -> Vec<StdinBufferEvent> {
        if self.buffer.is_empty() {
            Vec::new()
        } else {
            self.pending_kitty_printable_codepoint = None;
            vec![StdinBufferEvent::Data(std::mem::take(&mut self.buffer))]
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
    }

    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    pub fn destroy(&mut self) {
        self.clear();
    }
}

fn is_complete_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(ESC) {
        return SequenceStatus::NotEscape;
    }
    if data.len() == 1 {
        return SequenceStatus::Incomplete;
    }
    let after_esc = &data[1..];
    if after_esc.starts_with('[') {
        return is_complete_csi_sequence(data);
    }
    if after_esc.starts_with(']') {
        return if data.ends_with("\u{1b}\\") || data.ends_with('\u{7}') {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }
    if after_esc.starts_with('P') || after_esc.starts_with('_') {
        return if data.ends_with("\u{1b}\\") {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }
    if after_esc.starts_with('O') {
        return if after_esc.len() >= 2 {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }
    if after_esc.chars().count() == 1 {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Complete
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceStatus {
    Complete,
    Incomplete,
    NotEscape,
}

fn is_complete_csi_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\u{1b}[") {
        return SequenceStatus::Complete;
    }
    if data.len() < 3 {
        return SequenceStatus::Incomplete;
    }
    let payload = &data[2..];
    let Some(last) = payload.chars().last() else {
        return SequenceStatus::Incomplete;
    };
    let code = last as u32;
    if (0x40..=0x7e).contains(&code) {
        if payload.starts_with('<') {
            let body = &payload[1..payload.len().saturating_sub(1)];
            let valid_mouse = matches!(last, 'M' | 'm')
                && body.split(';').count() == 3
                && body
                    .split(';')
                    .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
            return if valid_mouse {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u32> {
    let body = sequence.strip_prefix("\u{1b}[")?.strip_suffix('u')?;
    if body.contains(';') {
        return None;
    }
    let codepoint = body.split(':').next()?.parse::<u32>().ok()?;
    (codepoint >= 32).then_some(codepoint)
}

fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let mut sequences = Vec::new();
    let mut pos = 0;
    while pos < buffer.len() {
        let remaining = &buffer[pos..];
        if remaining.starts_with(ESC) {
            let mut seq_end = 1;
            let mut completed = false;
            while seq_end <= remaining.len() {
                let candidate = &remaining[..seq_end];
                match is_complete_sequence(candidate) {
                    SequenceStatus::Complete => {
                        sequences.push(candidate.to_string());
                        pos += seq_end;
                        completed = true;
                        break;
                    }
                    SequenceStatus::Incomplete => seq_end += 1,
                    SequenceStatus::NotEscape => {
                        sequences.push(candidate.to_string());
                        pos += seq_end;
                        completed = true;
                        break;
                    }
                }
            }
            if !completed {
                return (sequences, remaining.to_string());
            }
        } else {
            let ch = remaining.chars().next().expect("non-empty");
            sequences.push(ch.to_string());
            pos += ch.len_utf8();
        }
    }
    (sequences, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_split_escape_sequence() {
        let mut buffer = StdinBuffer::default();
        assert!(buffer.process("\u{1b}").is_empty());
        assert_eq!(buffer.get_buffer(), "\u{1b}");
        assert_eq!(
            buffer.process("[A"),
            vec![StdinBufferEvent::Data("\u{1b}[A".into())]
        );
        assert_eq!(buffer.get_buffer(), "");
    }

    #[test]
    fn drops_duplicate_raw_char_after_kitty_printable() {
        let mut buffer = StdinBuffer::default();
        assert_eq!(
            buffer.process("\u{1b}[64u@"),
            vec![StdinBufferEvent::Data("\u{1b}[64u".into())]
        );
    }

    #[test]
    fn keeps_raw_char_after_modified_kitty_printable() {
        let mut buffer = StdinBuffer::default();
        assert_eq!(
            buffer.process("\u{1b}[64;3u@"),
            vec![
                StdinBufferEvent::Data("\u{1b}[64;3u".into()),
                StdinBufferEvent::Data("@".into())
            ]
        );
    }
}
