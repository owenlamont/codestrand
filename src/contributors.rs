use std::{
    collections::HashMap,
    fs, io,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::timeline::Event;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_CONTRIBUTORS: usize = 4096;
const MAX_ALIASES: usize = 4096;
const MAX_STRING_BYTES: usize = 4096;
const MAX_EXPANDED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Key {
    Email(Vec<u8>),
    Name(Vec<u8>),
    Config(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Contributor {
    pub key: Key,
    pub name: String,
    pub bot: Option<bool>,
    pub accounts: Accounts,
    pub portrait: Option<std::sync::Arc<crate::avatars::Portrait>>,
}

impl Contributor {
    pub(super) fn bot_rule(&self, email: &[u8]) -> &'static str {
        if let Some(bot) = self.bot {
            return if bot {
                "override_bot"
            } else {
                "override_human"
            };
        }
        if crate::avatars::noreply(&String::from_utf8_lossy(email))
            .is_some_and(|login| login.ends_with("[bot]"))
        {
            "github_noreply"
        } else if self.name.to_ascii_lowercase().ends_with("[bot]") {
            "name_suffix"
        } else {
            "human"
        }
    }

    pub(super) fn is_bot(&self, email: &[u8]) -> bool {
        matches!(
            self.bot_rule(email),
            "override_bot" | "github_noreply" | "name_suffix"
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Accounts {
    pub github: Option<String>,
    pub gitlab: Option<String>,
    pub avatar: Option<Avatar>,
    pub gravatar: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Avatar {
    Local(PathBuf, PathBuf),
    Remote(String),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("contributors file {} exceeds configuration limits", path.display())]
    Limit { path: PathBuf },
    #[error("contributors file {}: contributor {name:?}, {field}: {reason}", path.display())]
    Invalid {
        path: PathBuf,
        name: String,
        field: &'static str,
        reason: &'static str,
    },
    #[error("read contributors file {}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("parse contributors file {}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("contributors file {} lists email {email:?} more than once", path.display())]
    Duplicate { path: PathBuf, email: String },
}

#[derive(Debug, Default)]
pub(super) struct Mappings {
    emails: HashMap<Vec<u8>, Contributor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    contributor: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    bot: Option<bool>,
    emails: Vec<String>,
    github: Option<String>,
    gitlab: Option<String>,
    avatar: Option<String>,
    gravatar: Option<String>,
}

pub(super) fn load(explicit: Option<&Path>) -> Result<Mappings, Error> {
    let default =
        dirs::config_dir().map(|root| root.join("codestrand/contributors.toml"));
    load_path(explicit.or(default.as_deref()), explicit.is_some())
}

fn load_path(path: Option<&Path>, required: bool) -> Result<Mappings, Error> {
    let Some(path) = path else {
        return Ok(Mappings::default());
    };
    let text = match read_config(path) {
        Ok(text) => text,
        Err(source) if !required && source.kind() == io::ErrorKind::NotFound => {
            return Ok(Mappings::default());
        }
        Err(source) => {
            return Err(Error::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    Mappings::parse(&text, path)
}

fn read_config(path: &Path) -> io::Result<String> {
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "contributors file exceeds 1 MiB or is not a regular file",
        ));
    }
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "contributors file exceeds 1 MiB",
        ));
    }
    Ok(text)
}

fn avatar_path(value: &str, config: &Path) -> Result<Avatar, &'static str> {
    if value.contains("://") {
        return crate::avatars::https_url(value)
            .then(|| Avatar::Remote(value.to_owned()))
            .ok_or("expected an HTTPS URL or local image path");
    }
    if value.is_empty() {
        return Err("expected a nonempty local image path");
    }
    Ok(Avatar::Local(
        config.parent().unwrap_or(Path::new(".")).join(value),
        config.parent().unwrap_or(Path::new(".")).to_owned(),
    ))
}

impl Mappings {
    fn parse(text: &str, path: &Path) -> Result<Self, Error> {
        let limit = || Error::Limit {
            path: path.to_owned(),
        };
        if text.len() as u64 > MAX_CONFIG_BYTES {
            return Err(limit());
        }
        let config: Config = toml::from_str(text).map_err(|source| Error::Parse {
            path: path.to_owned(),
            source,
        })?;
        if config.contributor.len() > MAX_CONTRIBUTORS {
            return Err(limit());
        }
        let mut aliases = 0usize;
        let mut expanded = 0usize;
        for entry in &config.contributor {
            let fields = [&entry.name]
                .into_iter()
                .chain(entry.github.iter())
                .chain(entry.gitlab.iter())
                .chain(entry.avatar.iter())
                .chain(entry.gravatar.iter());
            let mut bytes = 0;
            for field in fields {
                if field.len() > MAX_STRING_BYTES {
                    return Err(limit());
                }
                bytes += field.len();
            }
            aliases += entry.emails.len();
            for email in &entry.emails {
                if email.len() > MAX_STRING_BYTES {
                    return Err(limit());
                }
                expanded = expanded.saturating_add(bytes + email.len());
            }
            if aliases > MAX_ALIASES || expanded > MAX_EXPANDED_BYTES {
                return Err(limit());
            }
        }
        let mut emails = HashMap::new();
        for (index, entry) in config.contributor.into_iter().enumerate() {
            let invalid = |field, reason| Error::Invalid {
                path: path.to_owned(),
                name: entry.name.clone(),
                field,
                reason,
            };
            if let Some(login) = &entry.github
                && !crate::avatars::valid_login(login)
            {
                return Err(invalid("github", "expected a GitHub login (not a URL)"));
            }
            if let Some(username) = &entry.gitlab
                && (username.is_empty()
                    || username.len() > 255
                    || !username
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    || !username.as_bytes()[0].is_ascii_alphanumeric())
            {
                return Err(invalid(
                    "gitlab",
                    "expected a GitLab username (letters, digits, _, - or .)",
                ));
            }
            let avatar = entry
                .avatar
                .as_ref()
                .map(|value| {
                    avatar_path(value, path).map_err(|reason| invalid("avatar", reason))
                })
                .transpose()?;
            for email in entry.emails {
                let key = email.as_bytes().to_ascii_lowercase();
                let contributor = Contributor {
                    key: Key::Config(index),
                    name: entry.name.clone(),
                    bot: entry.bot,
                    accounts: Accounts {
                        github: entry.github.clone(),
                        gitlab: entry.gitlab.clone(),
                        avatar: avatar.clone(),
                        gravatar: entry.gravatar.clone(),
                    },
                    portrait: None,
                };
                if emails.insert(key, contributor).is_some() {
                    return Err(Error::Duplicate {
                        path: path.to_owned(),
                        email,
                    });
                }
            }
        }
        Ok(Self { emails })
    }

    pub(super) fn apply(&self, events: &mut [Event]) {
        let mut names = HashMap::new();
        for event in events.iter() {
            names.insert(
                unlisted_key(event),
                String::from_utf8_lossy(&event.author.name).into_owned(),
            );
        }
        for event in events {
            let email = event.author.email.to_ascii_lowercase();
            event.contributor =
                Some(self.emails.get(&email).cloned().unwrap_or_else(|| {
                    let key = unlisted_key(event);
                    Contributor {
                        bot: None,
                        name: names[&key].clone(),
                        accounts: Accounts::default(),
                        portrait: None,
                        key,
                    }
                }));
        }
    }
}

fn unlisted_key(event: &Event) -> Key {
    if event.author.email.is_empty() {
        Key::Name(event.author.name.clone())
    } else {
        Key::Email(event.author.email.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_overrides_precede_noreply_and_suffix_without_substring_matches() {
        let mut history = events(&[
            (
                "Renamed automation",
                "42+dependabot[bot]@users.noreply.github.com",
            ),
            ("Human [bot] notes", "human@example.com"),
            (
                "Fallback[bot]",
                "invalid+login[bot]@users.noreply.github.com",
            ),
            ("Normal", "other@example.com"),
        ]);
        Mappings::default().apply(&mut history);
        let rules: Vec<_> = history
            .iter()
            .map(|e| e.contributor.as_ref().unwrap().bot_rule(&e.author.email))
            .collect();
        assert_eq!(rules, ["github_noreply", "human", "name_suffix", "human"]);
        for (value, expected) in [(false, "override_human"), (true, "override_bot")] {
            let mappings = Mappings::parse(&format!("[[contributor]]\nname='Renamed'\nemails=['42+dependabot[bot]@users.noreply.github.com']\nbot={value}"), Path::new("contributors.toml")).unwrap();
            mappings.apply(&mut history);
            assert_eq!(
                history[0]
                    .contributor
                    .as_ref()
                    .unwrap()
                    .bot_rule(&history[0].author.email),
                expected
            );
            assert_eq!(
                history[0]
                    .contributor
                    .as_ref()
                    .unwrap()
                    .is_bot(&history[0].author.email),
                value
            );
        }
    }

    #[test]
    fn comment_over_one_mib_is_rejected_and_one_at_the_cap_loads() {
        let root = crate::cache::test_directory().unwrap();
        let path = root.path().join("contributors.toml");
        fs::write(&path, format!("#{}", "x".repeat(1024 * 1024))).unwrap();
        let error =
            format!("{:#}", anyhow::Error::from(load(Some(&path)).unwrap_err()));
        assert!(error.contains("exceeds 1 MiB"), "{error}");
        fs::write(&path, format!("#{}", "x".repeat(1024 * 1024 - 1))).unwrap();
        load(Some(&path)).unwrap();
    }

    #[test]
    fn local_avatar_cannot_escape_config_directory() {
        let root = crate::cache::test_directory().unwrap();
        let outside = root.path().join("outside");
        let config = root.path().join("config");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&config).unwrap();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([10u8, 20, 30, 255]))
            .save(outside.join("avatar.png"))
            .unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(config.join("images"))
                .arg(&outside)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, config.join("images")).unwrap();
        for value in [
            "../outside/avatar.png".to_owned(),
            "images/avatar.png".to_owned(),
            outside
                .join("avatar.png")
                .to_str()
                .unwrap()
                .replace('\\', "/"),
        ] {
            let mappings = Mappings::parse(
                &format!(
                    "[[contributor]]\nname='Author'\nemails=['a@b']\navatar='{value}'"
                ),
                &config.join("contributors.toml"),
            )
            .unwrap();
            let mut history = events(&[("Author", "a@b")]);
            mappings.apply(&mut history);
            crate::avatars::Resolver::new(None::<crate::avatars::Http>, None, false, 0)
                .resolve(&mut history, None);
            assert!(
                history[0].contributor.as_ref().unwrap().portrait.is_none(),
                "{value}"
            );
        }
    }

    #[test]
    fn expanded_aliases_and_long_strings_are_rejected() {
        let aliases = (0..4097)
            .map(|i| format!("\"{i}@example.invalid\""))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            Mappings::parse(
                &format!("[[contributor]]\nname='Author'\nemails=[{aliases}]"),
                Path::new("contributors.toml")
            )
            .is_err()
        );
        assert!(
            Mappings::parse(
                &format!(
                    "[[contributor]]\nname='{}'\nemails=['a@b']",
                    "x".repeat(4097)
                ),
                Path::new("contributors.toml")
            )
            .is_err()
        );
    }
    use crate::{
        model::{Options, Simulation},
        theme::Theme,
        timeline::{self, Identity},
    };

    fn events(identities: &[(&str, &str)]) -> Vec<Event> {
        identities
            .iter()
            .map(|&(name, email)| Event {
                author: Identity {
                    name: name.as_bytes().to_vec(),
                    email: email.as_bytes().to_vec(),
                },
                repository: 0,
                contributor: None,
                commit: String::new(),
                seconds: 0,
                changes: vec![timeline::FileChange {
                    path: email.to_owned(),
                    kind: timeline::Kind::Modified,
                    insertions: 1,
                    removals: 0,
                    samples: Vec::new(),
                }],
            })
            .collect()
    }

    fn names(events: &[Event], mappings: &Mappings) -> Vec<String> {
        let mut sim = Simulation::new(
            events.to_vec(),
            &Options::default(),
            &Theme::neon(),
            mappings,
        );
        sim.advance_to(120, 30);
        sim.avatars()
            .iter()
            .map(|avatar| avatar.name.clone())
            .collect()
    }

    #[test]
    fn optional_platform_accounts_parse_and_follow_email_aliases() {
        let mappings = Mappings::parse(
            r#"
[[contributor]]
name = "Owen"
emails = ["one@example.invalid", "two@example.invalid"]
github = "owenlamont"
gitlab = "owen.lamont"
gravatar = "avatar@example.invalid"
"#,
            Path::new("contributors.toml"),
        )
        .unwrap();
        let mut events = events(&[
            ("One", "one@example.invalid"),
            ("Two", "two@example.invalid"),
        ]);
        mappings.apply(&mut events);
        let contributor = events[0].contributor.as_ref().unwrap();
        assert_eq!(contributor.accounts.github.as_deref(), Some("owenlamont"));
        assert_eq!(contributor.accounts.gitlab.as_deref(), Some("owen.lamont"));
        assert_eq!(
            contributor.accounts.gravatar.as_deref(),
            Some("avatar@example.invalid")
        );
        assert_eq!(events[0].contributor, events[1].contributor);
    }

    #[test]
    fn invalid_accounts_and_overrides_report_contributor_field_and_file() {
        for (field, value) in [
            ("github", "https://github.com/alice"),
            ("github", ""),
            ("gitlab", "alice&email=secret"),
            ("gitlab", ""),
            ("avatar", "http://example.invalid/avatar.png"),
            ("avatar", "https://"),
            ("avatar", "https://user:password@example.invalid/a.png"),
            ("avatar", ""),
        ] {
            let text = format!(
                "[[contributor]]\nname = 'Alice'\nemails = []\n{field} = '{value}'"
            );
            let error = Mappings::parse(&text, Path::new("aliases.toml")).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("aliases.toml"), "{message}");
            assert!(message.contains("Alice"), "{message}");
            assert!(message.contains(field), "{message}");
        }
    }

    #[test]
    fn local_override_resolves_relative_to_contributors_file() {
        let root = crate::cache::test_directory().unwrap();
        let image = root.path().join("portrait.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10u8, 20, 30, 255]))
            .save(&image)
            .unwrap();
        let mappings = Mappings::parse("[[contributor]]\nname = 'Alice'\nemails = ['a@example.invalid']\navatar = 'portrait.png'", &root.path().join("contributors.toml")).unwrap();
        assert_eq!(
            mappings.emails[b"a@example.invalid".as_slice()]
                .accounts
                .avatar
                .as_ref(),
            Some(&Avatar::Local(image.clone(), root.path().to_owned()))
        );
        fs::write(&image, b"invalid").unwrap();
        assert!(
            Mappings::parse(
                "[[contributor]]\nname = 'Alice'\nemails = []\navatar = 'portrait.png'",
                &root.path().join("contributors.toml")
            )
            .is_ok()
        );
    }
    #[cfg(windows)]
    #[test]
    fn local_override_preserves_non_unicode_parent() {
        use std::os::windows::ffi::OsStringExt;
        let parent = PathBuf::from(std::ffi::OsString::from_wide(&[0xD800]));
        assert_eq!(
            avatar_path("portrait.png", &parent.join("contributors.toml")).unwrap(),
            Avatar::Local(parent.join("portrait.png"), parent)
        );
    }

    #[test]
    fn two_emails_share_an_avatar_and_colour_identity() {
        let mappings = Mappings::parse(
            r#"
[[contributor]]
name = "Owen Lamont"
emails = ["personal@example.invalid", "work@example.invalid"]
"#,
            Path::new("contributors.toml"),
        )
        .unwrap();
        let mut events = events(&[
            ("Personal", "PERSONAL@example.invalid"),
            ("Work", "work@example.invalid"),
            ("Unknown", "unknown@example.invalid"),
        ]);
        let raw = events.clone();
        mappings.apply(&mut events);
        assert_eq!(events[0].contributor, events[1].contributor);
        assert_eq!(events[0].author, raw[0].author);
        assert_eq!(events[1].author, raw[1].author);
        assert_eq!(names(&events, &mappings), ["Owen Lamont", "Unknown"]);
    }

    #[test]
    fn identical_names_do_not_merge_unknown_emails() {
        let events = events(&[
            ("Same Name", "one@example.invalid"),
            ("Same Name", "two@example.invalid"),
        ]);
        assert_eq!(
            names(&events, &Mappings::default()),
            ["Same Name", "Same Name"]
        );
    }

    #[test]
    fn empty_emails_keep_different_authors_separate() {
        let events = events(&[("Alice", ""), ("Bob", "")]);
        assert_eq!(names(&events, &Mappings::default()), ["Alice", "Bob"]);
    }

    #[test]
    fn unknown_tables_and_fields_report_the_file_and_key() {
        for (text, key) in [
            ("[[contributors]]", "contributors"),
            (
                "[[contributor]]\nname = 'Alice'\nemails = []\nemials = []",
                "emials",
            ),
        ] {
            let error = Mappings::parse(text, Path::new("aliases.toml")).unwrap_err();
            assert!(matches!(error, Error::Parse { .. }));
            assert!(error.to_string().contains("aliases.toml"));
            let cause = std::error::Error::source(&error).unwrap().to_string();
            assert!(cause.contains("unknown field"), "{cause}");
            assert!(cause.contains(key), "{cause}");
        }
    }

    #[test]
    fn unknown_email_keeps_the_last_playback_name() {
        let mut events = events(&[
            ("Old", "same@example.invalid"),
            ("Recent", "SAME@example.invalid"),
            ("Unknown", "other@example.invalid"),
        ]);
        Mappings::default().apply(&mut events);
        assert_eq!(events[0].contributor.as_ref().unwrap().name, "Recent");
        assert_eq!(events[2].contributor.as_ref().unwrap().name, "Unknown");
        assert_eq!(names(&events, &Mappings::default()), ["Recent", "Unknown"]);
    }

    #[test]
    fn duplicate_email_reports_the_file_and_email() {
        let error = Mappings::parse(
            r#"
[[contributor]]
name = "One"
emails = ["same@example.invalid"]
[[contributor]]
name = "Two"
emails = ["SAME@example.invalid"]
"#,
            Path::new("aliases.toml"),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Duplicate { .. }));
        assert!(error.to_string().contains("aliases.toml"));
        assert!(error.to_string().contains("SAME@example.invalid"));
    }

    #[test]
    fn malformed_toml_reports_the_file_and_problem() {
        let error =
            Mappings::parse("[[contributor", Path::new("broken.toml")).unwrap_err();
        assert!(matches!(error, Error::Parse { .. }));
        assert!(error.to_string().contains("broken.toml"));
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .to_string()
                .contains("expected")
        );
    }

    #[test]
    fn only_missing_default_file_is_optional() {
        let scratch = crate::cache::test_directory().unwrap();
        let path = scratch.path().join("absent.toml");
        assert!(load_path(Some(&path), false).unwrap().emails.is_empty());
        let error = load(Some(&path)).unwrap_err();
        assert!(matches!(error, Error::Read { .. }));
        assert!(error.to_string().contains("absent.toml"));
        fs::write(&path, "[[contributor").unwrap();
        assert!(matches!(
            load_path(Some(&path), false),
            Err(Error::Parse { .. })
        ));
    }
}
