use serde::{Deserialize, Serialize};

pub(super) const PER_FILE: usize = 3;
pub(super) const PER_COMMIT: usize = 12;
const MAX_BYTES: usize = 72;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Sample {
    pub added: bool,
    pub text: String,
}

pub(super) fn consider(samples: &mut Vec<Sample>, bytes: &[u8], added: bool) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let text = text.trim();
    if text.len() > MAX_BYTES
        || text.chars().any(char::is_control)
        || text.chars().filter(|c| c.is_alphanumeric()).take(4).count() < 4
        || samples
            .iter()
            .any(|sample| sample.added == added && sample.text == text)
    {
        return;
    }
    samples.push(Sample {
        added,
        text: text.to_owned(),
    });
    samples.sort_by(|a, b| {
        (a.text.len(), &a.text, a.added).cmp(&(b.text.len(), &b.text, b.added))
    });
    let other = samples
        .iter()
        .position(|sample| sample.added != samples[0].added);
    if let Some(index) = other {
        let opposite = samples.remove(index);
        samples.insert(1, opposite);
    }
    samples.truncate(PER_FILE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_meaningful_lines_are_bounded_and_both_sides_survive() {
        let mut samples = Vec::new();
        for text in ["", "}", " // ", "\t", "abcd\u{1b}", &"x".repeat(73)] {
            consider(&mut samples, text.as_bytes(), true);
        }
        consider(&mut samples, &[0xff], true);
        assert_eq!(samples, []);
        for (text, added) in [
            ("let old = 1;", false),
            ("new();", true),
            ("abcd", true),
            ("abcde", true),
            ("abcdef", true),
        ] {
            consider(&mut samples, text.as_bytes(), added);
        }
        assert_eq!(samples.len(), PER_FILE);
        assert!(samples.iter().any(|sample| !sample.added));
        assert_eq!(samples[0].text, "abcd");
        let previous = samples.clone();
        consider(&mut samples, b"abcd", true);
        assert_eq!(samples, previous);
    }
}
