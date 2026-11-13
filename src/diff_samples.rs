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
    if bytes.len() > MAX_BYTES || !bytes.is_ascii() {
        return;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let text = text.trim();
    if sensitive_line(text)
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
        (rank(&a.text), &a.text, a.added).cmp(&(rank(&b.text), &b.text, b.added))
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

fn rank(text: &str) -> (bool, usize) {
    (!(16..=60).contains(&text.len()), text.len().abs_diff(36))
}

pub(super) fn sensitive_path(path: &[u8]) -> bool {
    path.split(|byte| *byte == b'/' || *byte == b'\\')
        .any(|part| {
            let part = String::from_utf8_lossy(part).to_ascii_lowercase();
            part.starts_with(".env")
                || std::path::Path::new(&part).extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("pem") || ext.eq_ignore_ascii_case("key")
                })
                || part.starts_with("id_rsa")
        })
}

fn sensitive_line(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "ghp_",
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "akia",
        "asia",
        "xoxb-",
        "xoxp-",
        "sk-",
        "private key",
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
        || (text.contains('@') && text.contains('.'))
}

pub(super) fn sample_visibility(on_screen_px: f32) -> f32 {
    let weight = ((on_screen_px - 6.0) / 8.0).clamp(0.0, 1.0);
    weight * weight * (3.0 - 2.0 * weight)
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
        assert_eq!(samples[0].text, "let old = 1;");
        let previous = samples.clone();
        consider(&mut samples, b"abcd", true);
        assert_eq!(samples, previous);
    }

    #[test]
    fn visibility_hides_overview_and_smoothly_reveals_detail() {
        for size in [0.0, 1.0, 4.0, 5.9, 6.0] {
            assert_eq!(sample_visibility(size), 0.0);
        }
        assert_eq!(sample_visibility(10.0), 0.5);
        assert_eq!(sample_visibility(14.0), 1.0);
        assert_eq!(sample_visibility(48.0), 1.0);
        let values: Vec<_> = (0_u16..=480)
            .map(|i| sample_visibility(f32::from(i) / 10.0))
            .collect();
        assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn sensitive_and_unsupported_text_is_omitted() {
        let mut samples = Vec::new();
        for text in [
            "token = \"ghp_example\"",
            "PASSWORD=example",
            "key = \"AKIA123456\"",
            "Authorization: Bearer example",
            "mail = person@example.com",
            "let café = 1;",
            "-----BEGIN PRIVATE KEY-----",
            "api_key = \"example\"",
            "xoxb-example",
        ] {
            consider(&mut samples, text.as_bytes(), true);
        }
        assert_eq!(samples, []);
        consider(&mut samples, b"let greeting = hello();", true);
        assert_eq!(samples.len(), 1);
        for path in [
            ".env",
            "config/.env.production",
            "cert.PEM",
            "ssh/id_rsa.pub",
            "a.key",
        ] {
            assert!(sensitive_path(path.as_bytes()), "{path}");
        }
        assert!(!sensitive_path(b"src/environment.rs"));
    }

    #[test]
    fn mid_length_lines_outrank_trivial_lines_in_any_order() {
        let lines = [
            "} else {",
            "return;",
            "let result = calculate_value();",
            "abcd",
            "run();",
        ];
        let mut forward = Vec::new();
        let mut reverse = Vec::new();
        for line in lines {
            consider(&mut forward, line.as_bytes(), true);
        }
        for line in lines.into_iter().rev() {
            consider(&mut reverse, line.as_bytes(), true);
        }
        assert_eq!(forward, reverse);
        assert_eq!(forward[0].text, "let result = calculate_value();");
    }
}
