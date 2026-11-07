use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{history, timeline};

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    tip: String,
    events: Vec<timeline::Event>,
}

pub(super) fn root() -> Option<PathBuf> {
    dirs::cache_dir().map(|path| path.join("codestrand"))
}

pub(super) fn load(
    path: &Path,
    revision: &str,
    cache_root: Option<&Path>,
) -> Result<Vec<timeline::Event>, history::Error> {
    let (repo, tip) = history::resolve(path, revision)?;
    let entry = cache_root
        .filter(|_| repo.objects.store_ref().replacements().next().is_none())
        .and_then(|root| entry_path(root, &repo));
    let tip_text = tip.to_string();
    if let Some(events) = entry.as_deref().and_then(|path| read(path, &tip_text)) {
        return Ok(events);
    }
    let history = history::import_resolved(&repo, tip)?;
    let snapshot = Snapshot {
        version: timeline::CACHE_VERSION,
        tip: tip_text,
        events: timeline::from_history(&history),
    };
    if let Some(path) = entry {
        let _ = store(&path, &snapshot);
    }
    Ok(snapshot.events)
}

fn entry_path(root: &Path, repo: &gix::Repository) -> Option<PathBuf> {
    let canonical = repo.common_dir().canonicalize().ok()?;
    let mut hash = gix::hash::hasher(gix::hash::Kind::Sha1);
    hash.update(canonical.as_os_str().as_encoded_bytes());
    let namespace = hash.try_finalize().ok()?.to_string();
    Some(root.join(namespace).join("timeline.json"))
}

fn read(path: &Path, tip: &str) -> Option<Vec<timeline::Event>> {
    let snapshot: Snapshot = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (snapshot.version == timeline::CACHE_VERSION && snapshot.tip == tip)
        .then_some(snapshot.events)
}

fn store(path: &Path, snapshot: &Snapshot) -> Option<()> {
    let parent = path.parent()?;
    fs::create_dir_all(parent).ok()?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).ok()?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(&mut writer, snapshot).ok()?;
        writer.flush().ok()?;
    }
    temporary.persist(path).ok()?;
    Some(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use clap::Parser;

    use super::*;

    fn git(path: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os()
            .filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        let output = command
            .arg("-C")
            .arg(path)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
            .env("GIT_COMMITTER_NAME", "Author")
            .env("GIT_COMMITTER_EMAIL", "author@example.invalid")
            .output()
            .expect("Git can start");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("fixture Git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn fixture() -> tempfile::TempDir {
        let scratch = tempfile::tempdir().expect("temporary directory is available");
        git(scratch.path(), &["init", "--initial-branch=main"]);
        git(scratch.path(), &["config", "commit.gpgsign", "false"]);
        git(
            scratch.path(),
            &["config", "core.hooksPath", "unused-hooks"],
        );
        commit(scratch.path(), "first\n");
        scratch
    }

    fn commit(path: &Path, text: &str) {
        fs::write(path.join("file.txt"), text).expect("fixture is writable");
        git(path, &["add", "file.txt"]);
        git(path, &["commit", "-m", "fixture"]);
    }

    fn entry(root: &Path, path: &Path) -> PathBuf {
        let (repo, _) = history::resolve(path, "HEAD").expect("fixture resolves");
        entry_path(root, &repo).expect("fixture path is canonical")
    }

    fn remove_blob(path: &Path) {
        let blob = git(path, &["rev-parse", "HEAD:file.txt"]);
        fs::remove_file(path.join(".git/objects").join(&blob[..2]).join(&blob[2..]))
            .expect("fixture blob is loose");
    }

    #[test]
    fn hit_returns_identical_events_without_importing_git() {
        let repo = fixture();
        let cache = tempfile::tempdir().expect("cache directory is available");
        let events =
            load(repo.path(), "HEAD", Some(cache.path())).expect("miss imports");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].changes[0].insertions, 1);
        assert!(entry(cache.path(), repo.path()).is_file());
        let tip = git(repo.path(), &["rev-parse", "HEAD"]);
        remove_blob(repo.path());
        assert!(load(repo.path(), "HEAD", None).is_err());
        assert_eq!(
            load(repo.path(), "HEAD", Some(cache.path())).expect("hit needs no blob"),
            events
        );
        assert_eq!(
            load(repo.path(), &tip, Some(cache.path())).expect("resolved tip hits"),
            events
        );
    }

    #[test]
    fn changed_contributors_file_applies_on_a_cache_hit() {
        let repo = fixture();
        let cache = tempfile::tempdir().unwrap();
        let config = cache.path().join("contributors.toml");
        let cli = crate::Cli::parse_from([
            "codestrand",
            "preview",
            "--repository",
            repo.path().to_str().unwrap(),
            "--contributors",
            config.to_str().unwrap(),
        ]);
        let Some(crate::Command::Preview { source, .. }) = cli.command else {
            panic!("preview command parsed")
        };
        for name in ["First", "Changed"] {
            let text = format!(
                r#"
[[contributor]]
name = "{name}"
emails = ["author@example.invalid"]
"#
            );
            fs::write(&config, text).unwrap();
            let mappings =
                crate::contributors::load(source.contributors.as_deref()).unwrap();
            let events = source.events(Some(cache.path())).unwrap();
            assert_eq!(events[0].author.name, b"Author");
            assert_eq!(events[0].author.email, b"author@example.invalid");
            assert!(events[0].contributor.is_none());
            let mut sim = crate::model::Simulation::new(
                events,
                &crate::model::Options::default(),
                &crate::theme::Theme::neon(),
                &mappings,
            );
            sim.advance_to(30, 30);
            assert_eq!(sim.avatars()[0].name, name);
            if name == "First" {
                remove_blob(repo.path());
                assert!(load(repo.path(), "HEAD", None).is_err());
            }
        }
    }

    #[test]
    fn new_commit_misses_and_replaces_snapshot() {
        let repo = fixture();
        let cache = tempfile::tempdir().expect("cache directory is available");
        load(repo.path(), "HEAD", Some(cache.path())).expect("first import");
        commit(repo.path(), "first\nsecond\n");
        let events =
            load(repo.path(), "HEAD", Some(cache.path())).expect("new tip imports");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].changes[0].insertions, 1);
        remove_blob(repo.path());
        assert_eq!(
            load(repo.path(), "HEAD", Some(cache.path())).expect("new tip cached"),
            events
        );
    }

    #[test]
    fn mismatched_or_corrupt_entries_regenerate() {
        let repo = fixture();
        let cache = tempfile::tempdir().expect("cache directory is available");
        let events =
            load(repo.path(), "HEAD", Some(cache.path())).expect("first import");
        let path = entry(cache.path(), repo.path());
        let valid = fs::read(&path).expect("snapshot exists");
        let mut wrong_version: serde_json::Value =
            serde_json::from_slice(&valid).expect("valid JSON");
        wrong_version["version"] = (timeline::CACHE_VERSION + 1).into();
        wrong_version["events"] = serde_json::json!([]);
        let mut wrong_tip = wrong_version.clone();
        wrong_tip["version"] = timeline::CACHE_VERSION.into();
        wrong_tip["tip"] = "other-tip".into();
        for invalid in [
            serde_json::to_vec(&wrong_version).unwrap(),
            serde_json::to_vec(&wrong_tip).unwrap(),
            b"{interrupted".to_vec(),
        ] {
            fs::write(&path, invalid).expect("snapshot is writable");
            assert_eq!(
                load(repo.path(), "HEAD", Some(cache.path()))
                    .expect("miss regenerates"),
                events
            );
            assert_eq!(fs::read(&path).expect("snapshot replaced"), valid);
        }
    }

    #[test]
    fn unavailable_cache_is_nonfatal() {
        let repo = fixture();
        let scratch = tempfile::tempdir().expect("temporary directory is available");
        let root = scratch.path().join("file");
        fs::write(&root, "not a directory").expect("fixture is writable");
        assert_eq!(
            load(repo.path(), "HEAD", Some(&root))
                .expect("cache failure imports")
                .len(),
            1
        );
    }

    #[test]
    fn replacement_objects_cannot_reuse_or_overwrite_a_snapshot() {
        let repo = fixture();
        let cache = tempfile::tempdir().expect("cache directory is available");
        let original = git(repo.path(), &["rev-parse", "HEAD"]);
        let events =
            load(repo.path(), &original, Some(cache.path())).expect("first import");
        commit(repo.path(), "replacement\ncontent\n");
        let replacement = git(
            repo.path(),
            &["commit-tree", "HEAD^{tree}", "-m", "replacement"],
        );
        git(repo.path(), &["replace", &original, &replacement]);
        let replaced = load(repo.path(), &original, Some(cache.path()))
            .expect("replacement imports");
        assert_ne!(replaced, events);
        assert_eq!(
            replaced,
            load(repo.path(), &original, None).expect("uncached replacement imports")
        );
        git(repo.path(), &["replace", "-d", &original]);
        remove_blob(repo.path());
        assert_eq!(
            load(repo.path(), &original, Some(cache.path()))
                .expect("original snapshot retained"),
            events
        );
    }

    #[test]
    fn bypass_flag_skips_reads_and_writes() {
        let repo = fixture();
        let cache = tempfile::tempdir().expect("cache directory is available");
        let cli = crate::Cli::parse_from([
            "codestrand",
            "preview",
            "--repository",
            repo.path().to_str().unwrap(),
            "--no-cache",
        ]);
        let Some(crate::Command::Preview { source, .. }) = cli.command else {
            panic!("preview command parsed")
        };
        let events = source.events(Some(cache.path())).expect("bypass imports");
        assert!(!entry(cache.path(), repo.path()).exists());
        load(repo.path(), "HEAD", Some(cache.path())).expect("seed cache");
        let before =
            fs::read(entry(cache.path(), repo.path())).expect("snapshot exists");
        assert_eq!(
            source
                .events(Some(cache.path()))
                .expect("bypass imports again"),
            events
        );
        assert_eq!(
            fs::read(entry(cache.path(), repo.path())).expect("snapshot exists"),
            before
        );
        remove_blob(repo.path());
        assert!(source.events(Some(cache.path())).is_err());
    }
}
