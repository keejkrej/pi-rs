#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, text: impl Into<String>, prepend: bool, accumulate: bool) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        if accumulate && !self.ring.is_empty() {
            let last = self.ring.pop().expect("ring not empty");
            self.ring.push(if prepend {
                format!("{text}{last}")
            } else {
                format!("{last}{text}")
            });
        } else {
            self.ring.push(text);
        }
    }

    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            let last = self.ring.pop().expect("ring not empty");
            self.ring.insert(0, last);
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}
