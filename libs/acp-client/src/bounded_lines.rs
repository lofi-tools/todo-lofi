const LINE_BYTES: usize = 4096;
const TRUNCATED_PREFIX: &str = "[… earlier stderr omitted …] ";

pub struct BoundedLine {
    pub text: String,
    pub truncated: bool,
}

#[derive(Default)]
pub struct BoundedLines {
    bytes: Vec<u8>,
    truncated: bool,
}

impl BoundedLines {
    pub fn push(&mut self, input: &[u8]) -> Vec<BoundedLine> {
        let mut lines = Vec::new();
        for fragment in input.split_inclusive(|byte| *byte == b'\n') {
            let complete = fragment.last() == Some(&b'\n');
            let content = if complete {
                &fragment[..fragment.len() - 1]
            } else {
                fragment
            };
            let excess = self
                .bytes
                .len()
                .saturating_add(content.len())
                .saturating_sub(LINE_BYTES);
            if excess > 0 {
                self.truncated = true;
                let remove = excess.min(self.bytes.len());
                self.bytes.drain(..remove);
                self.bytes.extend_from_slice(&content[excess - remove..]);
            } else {
                self.bytes.extend_from_slice(content);
            }
            if complete {
                lines.push(self.take());
            }
        }
        lines
    }

    pub fn finish(&mut self) -> Option<BoundedLine> {
        (!self.bytes.is_empty() || self.truncated).then(|| self.take())
    }

    fn take(&mut self) -> BoundedLine {
        if self.bytes.last() == Some(&b'\r') {
            self.bytes.pop();
        }
        let truncated = std::mem::take(&mut self.truncated);
        let text = String::from_utf8_lossy(&self.bytes);
        let text = if truncated {
            format!("{TRUNCATED_PREFIX}{text}")
        } else {
            text.into_owned()
        };
        self.bytes.clear();
        BoundedLine { text, truncated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_long_lines_and_keeps_following_lines_and_partial_utf8() {
        let mut lines = BoundedLines::default();
        for _ in 0..100 {
            assert!(lines.push(&[b'x'; 4096]).is_empty());
        }
        assert!(lines.bytes.len() <= LINE_BYTES);
        let result = lines.push(b"tail\nshort\r\n\xe7");
        assert!(lines.push(b"\x95\x8c").is_empty());
        assert_eq!(result.len(), 2);
        assert!(result[0].truncated);
        assert!(result[0].text.ends_with("tail"));
        assert_eq!(result[1].text, "short");
        assert_eq!(lines.finish().map(|line| line.text).as_deref(), Some("界"));
        assert!(lines.finish().is_none());
    }
}
