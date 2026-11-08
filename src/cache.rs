use std::{
    ffi::OsString,
    fs,
    io::{self, BufWriter, Read, Write},
    path::Component,
    path::{Path, PathBuf},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};

use crate::{history, timeline};

#[cfg(test)]
pub(super) fn test_directory() -> io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}

pub(super) struct ConfinedFile {
    directory: Dir,
    name: OsString,
}

impl ConfinedFile {
    pub(super) fn at(path: &Path, create: bool) -> io::Result<Self> {
        let absolute = std::path::absolute(path)?;
        let mut anchor = PathBuf::new();
        let mut parts = Vec::new();
        for component in absolute.components() {
            match component {
                Component::Prefix(prefix) => {
                    #[cfg(windows)]
                    if !matches!(
                        prefix.kind(),
                        std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                    ) {
                        return Err(io::Error::other(
                            "network and device paths are not allowed",
                        ));
                    }
                    anchor.push(prefix.as_os_str());
                }
                Component::RootDir => anchor.push(component.as_os_str()),
                Component::Normal(part) => parts.push(part.to_owned()),
                Component::CurDir => (),
                Component::ParentDir => {
                    return Err(io::Error::other("parent traversal is not allowed"));
                }
            }
        }
        let name = parts
            .pop()
            .ok_or_else(|| io::Error::other("expected a file path"))?;
        let mut directory =
            Dir::open_ambient_dir(anchor, cap_std::ambient_authority())?;
        for part in parts {
            if create {
                match directory.create_dir(&part) {
                    Ok(()) => (),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                    Err(error) => return Err(error),
                }
            }
            directory = directory.open_dir_nofollow(part)?;
        }
        Ok(Self { directory, name })
    }

    pub(super) fn open(&self) -> io::Result<fs::File> {
        let file = self.directory.open_with(
            &self.name,
            OpenOptions::new()
                .read(true)
                .follow(FollowSymlinks::No)
                .nonblock(true),
        )?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("expected a regular file"));
        }
        Ok(file.into_std())
    }

    pub(super) fn write(
        &self,
        write: impl FnOnce(&mut cap_std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        static NEXT: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let temporary = format!(
            ".codestrand-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut file = self.directory.open_with(
            &temporary,
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No),
        )?;
        let result = write(&mut file).and_then(|()| file.flush());
        drop(file);
        let result = result.and_then(|()| {
            self.directory
                .rename(&temporary, &self.directory, &self.name)
        });
        if result.is_err() {
            let _ = self.directory.remove_file(&temporary);
        }
        result
    }
}

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
    let mut file = ConfinedFile::at(path, false).ok()?.open().ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
    (snapshot.version == timeline::CACHE_VERSION && snapshot.tip == tip)
        .then_some(snapshot.events)
}
fn store(path: &Path, snapshot: &Snapshot) -> Option<()> {
    ConfinedFile::at(path, true)
        .ok()?
        .write(|file| {
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, snapshot)?;
            writer.flush()
        })
        .ok()?;
    Some(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use clap::Parser;

    use super::*;

    pub(crate) fn directory_link(target: &Path, link: &Path) {
        #[cfg(windows)]
        assert!(
            Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[test]
    fn linked_cache_namespace_cannot_redirect_reads_or_writes() {
        let repo = fixture();
        let root = crate::cache::test_directory().unwrap();
        let outside = crate::cache::test_directory().unwrap();
        let path = entry(root.path(), repo.path());
        directory_link(outside.path(), path.parent().unwrap());
        load(repo.path(), "HEAD", Some(root.path())).unwrap();
        assert!(!outside.path().join("timeline.json").exists());
        let events = load(repo.path(), "HEAD", None).unwrap();
        fs::write(
            outside.path().join("timeline.json"),
            serde_json::to_vec(&Snapshot {
                version: timeline::CACHE_VERSION,
                tip: git(repo.path(), &["rev-parse", "HEAD"]),
                events,
            })
            .unwrap(),
        )
        .unwrap();
        remove_blob(repo.path());
        assert!(load(repo.path(), "HEAD", Some(root.path())).is_err());
    }

    #[test]
    fn cache_root_and_ancestors_reject_directory_links() {
        let scratch = crate::cache::test_directory().unwrap();
        let outside = crate::cache::test_directory().unwrap();
        directory_link(outside.path(), &scratch.path().join("linked"));
        for suffix in ["file.json", "nested/file.json"] {
            let path = scratch.path().join("linked").join(suffix);
            assert!(ConfinedFile::at(&path, true).is_err());
            assert!(ConfinedFile::at(&path, false).is_err());
        }
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

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
        let scratch =
            crate::cache::test_directory().expect("temporary directory is available");
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
    fn opt_out_clears_samples_on_cache_hits_for_preview_and_export() {
        use clap::Parser;

        let repo = fixture();
        let cache = crate::cache::test_directory().unwrap();
        let original = load(repo.path(), "HEAD", Some(cache.path())).unwrap();
        assert_ne!(original[0].changes[0].samples, []);
        for command in [vec!["preview"], vec!["export", "frames"]] {
            let mut args = vec!["codestrand"];
            args.extend(command);
            args.extend([
                "--repository",
                repo.path().to_str().unwrap(),
                "--no-diff-samples",
            ]);
            let cli = crate::Cli::parse_from(args);
            let (crate::Command::Preview { source, .. }
            | crate::Command::Export { source, .. }) = cli.command.unwrap()
            else {
                panic!("playback command")
            };
            let events = source.events(Some(cache.path())).unwrap();
            assert!(
                events
                    .iter()
                    .flat_map(|event| &event.changes)
                    .all(|change| change.samples.is_empty())
            );
            assert_eq!(
                events[0].changes[0].insertions,
                original[0].changes[0].insertions
            );
        }
        assert_eq!(
            load(repo.path(), "HEAD", Some(cache.path())).unwrap(),
            original
        );
    }

    #[test]
    fn sensitive_paths_and_renames_keep_counts_without_samples() {
        let repo = fixture();
        for path in [".env.local", "server.pem", "server.key", "id_rsa_backup"] {
            fs::write(repo.path().join(path), "let ordinary = calculate();\n").unwrap();
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "sensitive paths"]);
        git(repo.path(), &["mv", ".env.local", "ordinary.txt"]);
        git(repo.path(), &["commit", "-m", "rename"]);
        let events = load(repo.path(), "HEAD", None).unwrap();
        assert_eq!(events[1].changes.len(), 4);
        assert!(
            events[1]
                .changes
                .iter()
                .all(|change| change.samples.is_empty() && change.insertions == 1)
        );
        assert!(
            events[2]
                .changes
                .iter()
                .all(|change| change.samples.is_empty())
        );
    }

    #[test]
    fn hit_returns_identical_events_without_importing_git() {
        let repo = fixture();
        let cache =
            crate::cache::test_directory().expect("cache directory is available");
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
        let cache = crate::cache::test_directory().unwrap();
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
        let cache =
            crate::cache::test_directory().expect("cache directory is available");
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
    #[ignore = "requires CODESTRAND_MEASURE_REPOSITORY and CODESTRAND_MEASURE_OUTPUT"]
    fn measure_sample_cache_growth() {
        let repository = std::env::var("CODESTRAND_MEASURE_REPOSITORY").unwrap();
        let output = PathBuf::from(std::env::var("CODESTRAND_MEASURE_OUTPUT").unwrap());
        let history = history::import(Path::new(&repository), "origin/main").unwrap();
        let snapshot = Snapshot {
            version: timeline::CACHE_VERSION,
            tip: history.tip.clone(),
            events: timeline::from_history(&history),
        };
        let after = serde_json::to_vec(&snapshot).unwrap();
        let mut old = serde_json::to_value(&snapshot).unwrap();
        old["version"] = 3.into();
        for event in old["events"].as_array_mut().unwrap() {
            for change in event["changes"].as_array_mut().unwrap() {
                change.as_object_mut().unwrap().remove("samples");
            }
        }
        let before = serde_json::to_vec(&old).unwrap();
        fs::write(output.join("cache-v3.json"), &before).unwrap();
        fs::write(output.join("cache-samples.json"), &after).unwrap();
        eprintln!(
            "tip={}, events={}, before_bytes={}, after_bytes={}",
            snapshot.tip,
            snapshot.events.len(),
            before.len(),
            after.len()
        );
        for event in &snapshot.events {
            assert!(
                event
                    .changes
                    .iter()
                    .map(|change| change.samples.len())
                    .sum::<usize>()
                    <= crate::diff_samples::PER_COMMIT
            );
        }
    }

    #[test]
    fn version_three_without_samples_reimports_actual_diff_lines() {
        let repo = fixture();
        commit(repo.path(), "first\nlet previous = 1;\n");
        commit(repo.path(), "first\nlet current = 2;\n");
        let cache = crate::cache::test_directory().unwrap();
        let events = load(repo.path(), "HEAD", Some(cache.path())).unwrap();
        let samples = &events.last().unwrap().changes[0].samples;
        assert!(
            samples
                .iter()
                .any(|sample| sample.added && sample.text == "let current = 2;")
        );
        assert!(
            samples
                .iter()
                .any(|sample| !sample.added && sample.text == "let previous = 1;")
        );
        assert!(samples.iter().all(|sample| sample.text != "first"));
        let path = entry(cache.path(), repo.path());
        let mut old: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["version"] = 3.into();
        for event in old["events"].as_array_mut().unwrap() {
            for change in event["changes"].as_array_mut().unwrap() {
                change.as_object_mut().unwrap().remove("samples");
            }
        }
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(
            load(repo.path(), "HEAD", Some(cache.path())).unwrap(),
            events
        );
        let rebuilt: serde_json::Value =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(rebuilt["version"], timeline::CACHE_VERSION);
    }

    #[test]
    fn large_commit_keeps_twelve_short_samples_and_cache_hits_preserve_them() {
        let repo = fixture();
        for index in 0..128 {
            fs::write(
                repo.path().join(format!("file-{index:03}.rs")),
                "\n}\nlet alpha = 1;\nlet beta = 2;\nlet gamma = 3;\n",
            )
            .unwrap();
        }
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "crowded"]);
        let cache = crate::cache::test_directory().unwrap();
        let events = load(repo.path(), "HEAD", Some(cache.path())).unwrap();
        let changes = &events.last().unwrap().changes;
        assert_eq!(changes.len(), 128);
        assert_eq!(
            changes
                .iter()
                .map(|change| change.samples.len())
                .sum::<usize>(),
            crate::diff_samples::PER_COMMIT
        );
        assert!(
            changes
                .iter()
                .all(|change| change.samples.len() <= crate::diff_samples::PER_FILE)
        );
        remove_blob(repo.path());
        assert_eq!(
            load(repo.path(), "HEAD", Some(cache.path())).unwrap(),
            events
        );
    }

    #[test]
    fn mismatched_or_corrupt_entries_regenerate() {
        let repo = fixture();
        let cache =
            crate::cache::test_directory().expect("cache directory is available");
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
        let scratch =
            crate::cache::test_directory().expect("temporary directory is available");
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
        let cache =
            crate::cache::test_directory().expect("cache directory is available");
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
        let cache =
            crate::cache::test_directory().expect("cache directory is available");
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
