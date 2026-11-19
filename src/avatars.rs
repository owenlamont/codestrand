use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{
    contributors::{Avatar, Key},
    timeline::Event,
};

const TTL: u64 = 7 * 24 * 60 * 60;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const CACHE_BYTES: u64 = 128 * 1024;
const FETCH_BUDGET: Duration = Duration::from_secs(30);
/// Account lookups stop while this much of the fetch budget remains, so images for
/// the accounts already found still get a share.
const IMAGE_RESERVE: Duration = Duration::from_secs(15);

pub(super) fn https_url(value: &str) -> bool {
    value.parse::<ureq::http::Uri>().is_ok_and(|uri| {
        uri.scheme_str() == Some("https")
            && uri.host().is_some_and(|host| !host.is_empty())
            && !uri
                .authority()
                .is_some_and(|authority| authority.as_str().contains('@'))
    })
}

fn local_portrait(path: &Path, root: &Path) -> std::io::Result<Option<Portrait>> {
    let allowed = std::path::absolute(root)
        .ok()
        .zip(std::path::absolute(path).ok())
        .is_some_and(|(root, path)| {
            path.starts_with(root)
                && !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        });
    if !allowed {
        return Err(std::io::Error::other(
            "avatar path escapes contributors directory",
        ));
    }
    let file = match crate::cache::ConfinedFile::at(path, false)
        .and_then(|file| file.open())
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Ok(None);
    }
    Ok(Portrait::decode(&bytes))
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Portrait {
    pub pixels: Vec<u8>,
}

impl Portrait {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(2048);
        limits.max_image_height = Some(2048);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?;
        Some(Self {
            pixels: image
                .resize_to_fill(128, 128, image::imageops::FilterType::Triangle)
                .into_rgba8()
                .into_raw(),
        })
    }
}

/// The GitHub account a commit email belongs to. `login` is lowercase; `id` is the
/// numeric account ID, which survives a rename.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct Account {
    pub login: String,
    pub id: Option<u64>,
}

impl Account {
    fn image_url(&self) -> String {
        if let Some(id) = self.id {
            return format!("https://avatars.githubusercontent.com/u/{id}?s=128");
        }
        let encoded = self.login.replace('[', "%5B").replace(']', "%5D");
        format!("https://github.com/{encoded}.png?size=128")
    }
}

/// A cached commit API answer: an account found for an email holds for every
/// repository, and a miss, stored without one, only for the remote that was asked.
#[derive(Serialize, Deserialize)]
struct Lookup {
    checked: u64,
    account: Option<Account>,
}

#[derive(Serialize, Deserialize)]
struct Image {
    checked: u64,
    image: bool,
}

#[derive(Deserialize)]
struct ApiAccount {
    login: String,
    id: u64,
}

#[derive(Deserialize)]
struct ApiCommit {
    sha: String,
    author: Option<ApiAccount>,
}

/// A lookup whose answer is not known: the network failed or was rate limited.
struct Unknown;

pub(super) enum Response {
    Found(Vec<u8>),
    Missing,
    Unavailable,
    RateLimited,
}

pub(super) trait Network {
    fn get(&mut self, url: &str, github_api: bool) -> Response;

    /// Time left in the fetch budget.
    fn remaining(&self) -> Duration {
        Duration::MAX
    }

    fn summary(&self, _summary: &Summary) {}
}

pub(super) struct Summary {
    pub contributors: usize,
    pub portraits: usize,
    pub unresolved: usize,
    pub unsupported_remotes: usize,
}

pub(super) struct Http {
    #[expect(clippy::disallowed_types, reason = "opt-in avatar HTTP client")]
    client: ureq::Agent,
    token: Option<String>,
    deadline: Instant,
    dead: HashSet<String>,
    failures: usize,
    limited: usize,
    skipped: usize,
}

impl Http {
    #[expect(
        clippy::disallowed_types,
        clippy::disallowed_methods,
        reason = "construct the opt-in avatar HTTP client"
    )]
    pub(super) fn new() -> Self {
        Self {
            client: ureq::Agent::config_builder()
                .https_only(true)
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .provider(ureq::tls::TlsProvider::Rustls)
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .build(),
                )
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(2)))
                .timeout_resolve(Some(Duration::from_secs(2)))
                .timeout_global(Some(Duration::from_secs(5)))
                .user_agent(concat!(
                    "codestrand/",
                    env!("CARGO_PKG_VERSION"),
                    " avatar-fetch"
                ))
                .build()
                .into(),
            token: std::env::var("GH_TOKEN")
                .or_else(|_| std::env::var("GITHUB_TOKEN"))
                .ok()
                .filter(|value| !value.is_empty()),
            deadline: Instant::now() + FETCH_BUDGET,
            dead: HashSet::new(),
            failures: 0,
            limited: 0,
            skipped: 0,
        }
    }
}

impl Network for Http {
    fn get(&mut self, url: &str, github_api: bool) -> Response {
        let host = url
            .split('/')
            .nth(2)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let remaining = self.remaining();
        if self.dead.contains(&host) || remaining.is_zero() {
            self.skipped += 1;
            return Response::Unavailable;
        }
        let mut request = self
            .client
            .get(url)
            .config()
            .timeout_global(Some(remaining.min(Duration::from_secs(5))))
            .build();
        if github_api {
            request = request
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(token) = &self.token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
        }
        let Ok(mut response) = request.call() else {
            self.dead.insert(host);
            self.failures += 1;
            return Response::Unavailable;
        };
        let result =
            response_body(response.status().as_u16(), response.body_mut().as_reader());
        match &result {
            Response::RateLimited => {
                self.dead.insert(host);
                self.limited += 1;
            }
            Response::Unavailable => {
                self.dead.insert(host);
                self.failures += 1;
            }
            Response::Found(_) | Response::Missing => {}
        }
        result
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    fn summary(&self, summary: &Summary) {
        eprintln!(
            "avatars: contributors={} portraits={} unresolved_emails={} failures={} limited={} skipped={} unsupported_remotes={}",
            summary.contributors,
            summary.portraits,
            summary.unresolved,
            self.failures,
            self.limited,
            self.skipped,
            summary.unsupported_remotes
        );
    }
}

impl Network for Option<Http> {
    fn get(&mut self, url: &str, github_api: bool) -> Response {
        self.as_mut()
            .map_or(Response::Unavailable, |http| http.get(url, github_api))
    }

    fn remaining(&self) -> Duration {
        self.as_ref().map_or(Duration::ZERO, Network::remaining)
    }

    fn summary(&self, summary: &Summary) {
        if let Some(http) = self {
            http.summary(summary);
        }
    }
}

fn response_body(status: u16, body: impl Read) -> Response {
    match status {
        401 | 403 | 429 => Response::RateLimited,
        404 | 422 => Response::Missing,
        200 => {
            let mut bytes = Vec::new();
            if body.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err()
                || u64::try_from(bytes.len()).expect("length fits u64") > MAX_BYTES
            {
                Response::Unavailable
            } else {
                Response::Found(bytes)
            }
        }
        _ => Response::Unavailable,
    }
}

/// Names a cache file; never sent anywhere.
fn hash(value: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    Sha256::digest(value)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
}

/// The account in a GitHub noreply address, `<id>+<login>@users.noreply.github.com`
/// or the older `<login>@users.noreply.github.com`.
pub(super) fn noreply(email: &str) -> Option<Account> {
    let (local, host) = email.trim().rsplit_once('@')?;
    if !host.eq_ignore_ascii_case("users.noreply.github.com") {
        return None;
    }
    let (id, login) = if let Some((id, login)) = local.split_once('+') {
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        (Some(id.parse().ok()?), login)
    } else {
        (None, local)
    };
    valid_login(login).then(|| Account {
        login: login.to_ascii_lowercase(),
        id,
    })
}

pub(super) fn valid_login(login: &str) -> bool {
    let name = login.strip_suffix("[bot]").unwrap_or(login);
    !name.is_empty()
        && name.len() <= 39
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

pub(super) fn github_remote(url: &str) -> Option<String> {
    let (host, path) = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        rest.split_once('/')?
    } else {
        url.split_once(':')?
    };
    if !host.rsplit('@').next()?.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_end_matches('/'));
    let (owner, repo) = path.split_once('/')?;
    if [owner, repo].iter().any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    }) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

pub(super) fn repository(path: &Path) -> Option<String> {
    let repo = gix::open(path).ok()?;
    let config = repo.config_snapshot();
    if let Some(url) = config.string("remote.origin.url")
        && let Some(remote) = github_remote(&url.to_string())
    {
        return Some(remote);
    }
    repo.remote_names().into_iter().find_map(|name| {
        let key = format!("remote.{name}.url");
        config
            .string(key.as_str())
            .and_then(|url| github_remote(&url.to_string()))
    })
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The commit email as matched everywhere: trimmed and lowercased.
pub(super) fn email_key(email: &[u8]) -> String {
    String::from_utf8_lossy(email).trim().to_ascii_lowercase()
}

/// Where one contributor's portrait comes from, best first.
#[derive(Default)]
struct Choice {
    avatar: Option<Avatar>,
    /// Rank 0 is a configured login, 1 an inferred account with a numeric ID, 2 one
    /// without.
    github: Option<(u8, Account)>,
}

pub(super) struct Resolver<N> {
    pub network: N,
    pub root: Option<PathBuf>,
    pub fetch: bool,
    pub now: u64,
    limited: bool,
    found: HashMap<String, Account>,
    unresolved: HashSet<String>,
    unsupported_remotes: usize,
    memo: HashMap<String, Option<Arc<Portrait>>>,
}

impl<N: Network> Resolver<N> {
    pub(super) fn new(
        network: N,
        root: Option<PathBuf>,
        fetch: bool,
        now: u64,
    ) -> Self {
        Self {
            network,
            root,
            fetch,
            now,
            limited: false,
            found: HashMap::new(),
            unresolved: HashSet::new(),
            unsupported_remotes: 0,
            memo: HashMap::new(),
        }
    }

    /// The GitHub account found for each commit email so far.
    pub(super) fn found(&self) -> &HashMap<String, Account> {
        &self.found
    }

    /// Finds the GitHub account behind each commit email of one repository: from a
    /// noreply address, from the cache, or, when fetching, from the commit API for a
    /// github.com `remote`. Emails with a configured login are skipped.
    pub(super) fn accounts<'a>(
        &mut self,
        events: impl IntoIterator<Item = &'a Event>,
        remote: Option<&str>,
    ) {
        if remote.is_none() {
            self.unsupported_remotes += 1;
        }
        let mut pending = HashMap::<String, (usize, usize, &str)>::new();
        for (index, event) in events.into_iter().enumerate() {
            let Some(contributor) = &event.contributor else {
                continue;
            };
            let email = email_key(&event.author.email);
            if contributor.accounts.github.is_some()
                || matches!(contributor.accounts.avatar, Some(Avatar::Remote(_)))
                || email.is_empty()
                || self.found.contains_key(&email)
            {
                continue;
            }
            if let Some(account) = noreply(&email) {
                self.found.insert(email, account);
                continue;
            }
            pending.entry(email).or_insert((0, index, &event.commit)).0 += 1;
        }
        let mut pending: Vec<_> = pending.into_iter().collect();
        pending.sort_by_key(|(_, (commits, first, _))| (usize::MAX - commits, *first));
        let total = pending.len();
        for (processed, (email, (_, _, commit))) in pending.into_iter().enumerate() {
            if self.fetch && remote.is_some() {
                eprintln!(
                    "avatars: looking up account {}/{total} (30s fetch budget)",
                    processed + 1
                );
            }
            let account = self.account(&email, remote, commit);
            if let Some(account) = account {
                self.unresolved.remove(&email);
                self.found.insert(email, account);
            } else {
                self.unresolved.insert(email);
            }
        }
    }

    fn account(
        &mut self,
        email: &str,
        remote: Option<&str>,
        commit: &str,
    ) -> Option<Account> {
        let fresh = |record: &Lookup| {
            !self.fetch || self.now.saturating_sub(record.checked) < TTL
        };
        let found_key = format!("account\n{email}");
        let found = self.read::<Lookup>(&found_key).filter(|record| {
            record
                .account
                .as_ref()
                .is_some_and(|account| valid_login(&account.login))
        });
        let stale = found.as_ref().and_then(|record| {
            record.account.clone().map(|account| Account {
                login: account.login.to_ascii_lowercase(),
                id: account.id,
            })
        });
        if found.as_ref().is_some_and(fresh) {
            return stale;
        }
        let Some(remote) = remote else {
            return stale;
        };
        let miss_key = format!("miss\n{email}\n{remote}");
        if self.read::<Lookup>(&miss_key).as_ref().is_some_and(fresh)
            || !self.fetch
            || self.limited
            || commit.is_empty()
            || self.network.remaining() <= IMAGE_RESERVE
        {
            return stale;
        }
        match self.commit_author(remote, commit) {
            Ok(account) => {
                let record = Lookup {
                    checked: self.now,
                    account: account.clone(),
                };
                // A confirmed miss also clears the account, which GitHub no longer links.
                let _ = self.store(&found_key, &record);
                if account.is_none() {
                    let _ = self.store(&miss_key, &record);
                }
                account
            }
            Err(Unknown) => stale,
        }
    }

    /// The author account of `commit`, or `None` when GitHub returns the commit with no
    /// linked author; any other answer, such as a commit never pushed, is unknown.
    fn commit_author(
        &mut self,
        remote: &str,
        commit: &str,
    ) -> Result<Option<Account>, Unknown> {
        let bytes = match self.network.get(
            &format!(
                "https://api.github.com/repos/{remote}/commits?sha={commit}&per_page=1"
            ),
            true,
        ) {
            Response::Found(bytes) => bytes,
            Response::Missing | Response::Unavailable => return Err(Unknown),
            Response::RateLimited => {
                self.limited = true;
                return Err(Unknown);
            }
        };
        let commits =
            serde_json::from_slice::<Vec<ApiCommit>>(&bytes).map_err(|_| Unknown)?;
        let found = commits
            .into_iter()
            .next()
            .filter(|found| found.sha.eq_ignore_ascii_case(commit))
            .ok_or(Unknown)?;
        found
            .author
            .map(|author| {
                valid_login(&author.login)
                    .then(|| Account {
                        login: author.login.to_ascii_lowercase(),
                        id: Some(author.id),
                    })
                    .ok_or(Unknown)
            })
            .transpose()
    }

    /// Sets each contributor's portrait: its `avatar` override, else the image of its
    /// configured or found GitHub account.
    pub(super) fn portraits(&mut self, events: &mut [Event]) {
        let mut order = Vec::new();
        let mut choices = HashMap::<Key, Choice>::new();
        for event in events.iter() {
            let Some(contributor) = &event.contributor else {
                continue;
            };
            let choice = choices.entry(contributor.key.clone()).or_insert_with(|| {
                order.push(contributor.key.clone());
                Choice {
                    avatar: contributor.accounts.avatar.clone(),
                    github: None,
                }
            });
            let candidate = if let Some(login) = &contributor.accounts.github {
                Some((
                    0,
                    Account {
                        login: login.to_ascii_lowercase(),
                        id: None,
                    },
                ))
            } else {
                self.found
                    .get(&email_key(&event.author.email))
                    .map(|account| {
                        (2 - u8::from(account.id.is_some()), account.clone())
                    })
            };
            if let Some(candidate) = candidate
                && choice
                    .github
                    .as_ref()
                    .is_none_or(|best| candidate.0 < best.0)
            {
                choice.github = Some(candidate);
            }
        }
        let total = order.len();
        let mut portraits = HashMap::new();
        for (processed, key) in order.into_iter().enumerate() {
            if self.fetch {
                eprintln!("avatars: fetching portrait {}/{total}", processed + 1);
            }
            let choice = &choices[&key];
            if let Some(portrait) = self.portrait_for(choice) {
                portraits.insert(key, portrait);
            }
        }
        self.network.summary(&Summary {
            contributors: total,
            portraits: portraits.len(),
            unresolved: self.unresolved.len(),
            unsupported_remotes: self.unsupported_remotes,
        });
        for event in events {
            if let Some(contributor) = &mut event.contributor {
                contributor.portrait = portraits.get(&contributor.key).cloned();
            }
        }
    }

    fn portrait_for(&mut self, choice: &Choice) -> Option<Arc<Portrait>> {
        match &choice.avatar {
            Some(Avatar::Remote(url)) => return self.image(url),
            Some(Avatar::Local(path, root)) => match local_portrait(path, root) {
                Ok(Some(portrait)) => return Some(Arc::new(portrait)),
                Ok(None) => eprintln!(
                    "avatars: local override {} unavailable; trying GitHub",
                    crate::terminal_escape(&path.to_string_lossy())
                ),
                Err(_) => {
                    eprintln!(
                        "avatars: rejected local override {}; using default portrait",
                        crate::terminal_escape(&path.to_string_lossy())
                    );
                    return None;
                }
            },
            None => {}
        }
        let (_, account) = choice.github.as_ref()?;
        self.image(&account.image_url())
    }

    fn image(&mut self, url: &str) -> Option<Arc<Portrait>> {
        if let Some(portrait) = self.memo.get(url) {
            return portrait.clone();
        }
        let key = format!("image\n{url}");
        let cached = self.read::<Image>(&key);
        let stale = cached
            .as_ref()
            .filter(|record| record.image)
            .and_then(|_| self.cached_portrait(&key));
        let portrait = if !self.fetch
            || cached.as_ref().is_some_and(|record| {
                self.now.saturating_sub(record.checked) < TTL
                    && (!record.image || stale.is_some())
            }) {
            stale
        } else {
            self.download(&key, url, stale)
        };
        self.memo.insert(url.to_owned(), portrait.clone());
        portrait
    }

    fn download(
        &mut self,
        key: &str,
        url: &str,
        stale: Option<Arc<Portrait>>,
    ) -> Option<Arc<Portrait>> {
        let png = match self.network.get(url, false) {
            Response::Found(bytes) => {
                let png = (|| {
                    let portrait = Portrait::decode(&bytes)?;
                    let image = image::RgbaImage::from_raw(128, 128, portrait.pixels)?;
                    let mut png = Cursor::new(Vec::new());
                    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
                    Some(png.into_inner())
                })();
                if png.is_none() && stale.is_some() {
                    // An undecodable refresh keeps the previous portrait and retries.
                    return stale;
                }
                png
            }
            Response::Missing => None,
            Response::Unavailable | Response::RateLimited => return stale,
        };
        let record = Image {
            checked: self.now,
            image: png.is_some(),
        };
        if let Some(png) = &png {
            let _ = self.store_png(key, png);
        }
        let _ = self.store(key, &record);
        png.as_deref().and_then(Portrait::decode).map(Arc::new)
    }

    fn read<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let path = self.path(key)?;
        let file = crate::cache::ConfinedFile::at(&path, false)
            .ok()?
            .open()
            .ok()?;
        serde_json::from_reader(file.take(4096)).ok()
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        Some(
            self.root
                .as_ref()?
                .join("avatars-v3")
                .join(format!("{}.json", hash(key))),
        )
    }

    fn store(&self, key: &str, record: &impl Serialize) -> Option<()> {
        crate::cache::ConfinedFile::at(&self.path(key)?, true)
            .ok()?
            .write(|file| Ok(serde_json::to_writer(file, record)?))
            .ok()
    }

    fn store_png(&self, key: &str, png: &[u8]) -> Option<()> {
        crate::cache::ConfinedFile::at(&self.path(key)?.with_extension("png"), true)
            .ok()?
            .write(|file| file.write_all(png))
            .ok()
    }

    fn cached_portrait(&self, key: &str) -> Option<Arc<Portrait>> {
        let path = self.path(key)?.with_extension("png");
        let file = crate::cache::ConfinedFile::at(&path, false)
            .ok()?
            .open()
            .ok()?;
        let mut bytes = Vec::new();
        file.take(CACHE_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if u64::try_from(bytes.len()).ok()? > CACHE_BYTES {
            return None;
        }
        let mut reader = image::ImageReader::with_format(
            Cursor::new(bytes),
            image::ImageFormat::Png,
        );
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(128);
        limits.max_image_height = Some(128);
        reader.limits(limits);
        let image = reader.decode().ok()?;
        (image.width() == 128 && image.height() == 128).then(|| {
            Arc::new(Portrait {
                pixels: image.into_rgba8().into_raw(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, fs};

    use super::*;
    use crate::contributors::{Accounts, Contributor};

    const COMMIT: &str = "0123456789012345678901234567890123456789";
    const EXPLICIT: &str = "https://github.com/explicit.png?size=128";

    #[test]
    fn linked_avatar_cache_does_not_write_outside() {
        let root = crate::cache::test_directory().unwrap();
        let outside = crate::cache::test_directory().unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(root.path().join("avatars-v3"))
                .arg(outside.path())
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("avatars-v3"))
            .unwrap();
        let resolver = resolver(Some(root.path().to_owned()), false, vec![]);
        resolver.store(
            "test",
            &Image {
                checked: 0,
                image: false,
            },
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn hash_is_lowercase_sha256_hex() {
        assert_eq!(
            hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    struct Fake {
        replies: VecDeque<Response>,
        calls: Vec<(String, bool)>,
        remaining: Duration,
    }

    impl Network for Fake {
        fn get(&mut self, url: &str, api: bool) -> Response {
            self.calls.push((url.to_owned(), api));
            self.replies
                .pop_front()
                .expect("each network call has a scripted response")
        }

        fn remaining(&self) -> Duration {
            self.remaining
        }
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("transport failed"))
        }
    }

    #[expect(clippy::disallowed_types, reason = "local HTTP test server")]
    fn serve_once(listener: &std::net::TcpListener, status: u16) {
        let _ = serve_body(listener, status, b"ok");
    }

    #[expect(clippy::disallowed_types, reason = "local HTTP test server")]
    fn serve_body(
        listener: &std::net::TcpListener,
        status: u16,
        body: &[u8],
    ) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "local request arrived within five seconds"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("local listener failed: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 8192, "local headers fit eight KiB");
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        write!(
            socket,
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        socket.write_all(body).unwrap();
        String::from_utf8(request).unwrap()
    }

    fn png() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn commit_by(login: &str, id: u64) -> Response {
        Response::Found(
            format!(
                r#"[{{"sha":"{COMMIT}","author":{{"login":"{login}","id":{id}}}}}]"#
            )
            .into_bytes(),
        )
    }

    fn event(email: &str, github: Option<&str>) -> Event {
        Event {
            author: crate::timeline::Identity {
                name: b"Author".to_vec(),
                email: email.as_bytes().to_vec(),
            },
            repository: 0,
            contributor: Some(Contributor {
                bot: None,
                key: Key::Email(email.as_bytes().to_vec()),
                name: "Author".to_owned(),
                accounts: Accounts {
                    github: github.map(str::to_owned),
                    ..Accounts::default()
                },
                portrait: None,
            }),
            commit: COMMIT.to_owned(),
            seconds: 0,
            changes: vec![],
        }
    }

    fn with_avatar(email: &str, avatar: Avatar) -> Event {
        let mut source = event(email, None);
        source.contributor.as_mut().unwrap().accounts.avatar = Some(avatar);
        source
    }

    fn resolver(
        root: Option<PathBuf>,
        fetch: bool,
        replies: Vec<Response>,
    ) -> Resolver<Fake> {
        Resolver::new(
            Fake {
                replies: replies.into(),
                calls: vec![],
                remaining: Duration::MAX,
            },
            root,
            fetch,
            100,
        )
    }

    /// Account lookup then portraits, without merging contributors.
    fn resolve<N: Network>(
        resolver: &mut Resolver<N>,
        events: &mut [Event],
        remote: Option<&str>,
    ) {
        resolver.accounts(events.iter(), remote);
        resolver.portraits(events);
    }

    fn visible(event: &Event) -> bool {
        event.contributor.as_ref().unwrap().portrait.is_some()
    }

    #[test]
    #[expect(
        clippy::disallowed_types,
        clippy::disallowed_methods,
        reason = "exercise the avatar HTTP client against a local test server"
    )]
    fn token_reaches_only_the_github_api_over_local_http() {
        struct Local {
            http: Http,
            base: String,
        }
        impl Network for Local {
            fn get(&mut self, url: &str, github_api: bool) -> Response {
                let uri: ureq::http::Uri = url.parse().unwrap();
                self.http.get(
                    &format!("{}{}", self.base, uri.path_and_query().unwrap()),
                    github_api,
                )
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let api = serve_body(
                &listener,
                200,
                format!(
                    r#"[{{"sha":"{COMMIT}","author":{{"login":"alice","id":5}}}}]"#
                )
                .as_bytes(),
            );
            let image = serve_body(&listener, 200, &png());
            assert!(api.starts_with(&format!(
                "GET /repos/owner/repo/commits?sha={COMMIT}&per_page=1 "
            )));
            assert!(api.contains("secret-token"));
            assert!(image.starts_with("GET /u/5?s=128 "));
            assert!(!image.to_ascii_lowercase().contains("authorization:"));
        });
        let mut http = Http::new();
        http.token = Some("secret-token".to_owned());
        http.client = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let mut resolver = Resolver::new(Local { http, base }, None, true, 100);
        let mut events = [event("a@b", None)];
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        server.join().unwrap();
        assert!(visible(&events[0]));
    }

    #[test]
    fn no_request_carries_an_email_or_its_hash() {
        let emails = ["first@example.invalid", "second@example.invalid"];
        let mut events: Vec<_> =
            emails.iter().map(|email| event(email, None)).collect();
        let mut resolver = resolver(
            None,
            true,
            vec![
                commit_by("first", 1),
                Response::Missing,
                Response::Found(png()),
            ],
        );
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert_eq!(resolver.network.calls.len(), 3);
        for (url, _) in &resolver.network.calls {
            for email in emails {
                for secret in [email.to_owned(), hash(email)] {
                    assert!(!url.contains(&secret), "{url}");
                }
            }
        }
    }

    #[test]
    fn explicit_overrides_win_and_local_files_work_without_cache_or_network() {
        let root = crate::cache::test_directory().unwrap();
        let path = root.path().join("portrait.png");
        fs::write(&path, png()).unwrap();
        let mut local = resolver(None, false, vec![]);
        let mut events = [with_avatar(
            "author@example.invalid",
            Avatar::Local(path.clone(), path.parent().unwrap().to_owned()),
        )];
        resolve(&mut local, &mut events, Some("owner/repo"));
        assert!(visible(&events[0]));
        let source = with_avatar(
            "author@example.invalid",
            Avatar::Remote("https://example.invalid/portrait.png".to_owned()),
        );
        for response in [Response::Missing, Response::Found(png())] {
            let mut remote = resolver(None, true, vec![response]);
            resolve(&mut remote, &mut [source.clone()], Some("owner/repo"));
            assert_eq!(
                remote.network.calls,
                [("https://example.invalid/portrait.png".to_owned(), false)]
            );
        }
    }

    #[test]
    fn remote_override_cache_refresh_retains_failures_and_clears_confirmed_misses() {
        let root = crate::cache::test_directory().unwrap();
        let url = "https://example.invalid/portrait.png";
        let source = with_avatar("a@example.invalid", Avatar::Remote(url.to_owned()));
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        resolve(&mut initial, &mut [source.clone()], None);
        let mut events = [with_avatar(
            "changed@example.invalid",
            Avatar::Remote(url.to_owned()),
        )];
        let mut offline = resolver(Some(root.path().to_owned()), false, vec![]);
        offline.now += TTL;
        resolve(&mut offline, &mut events, Some("changed/repository"));
        assert!(visible(&events[0]));
        let mut failed = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Unavailable],
        );
        failed.now += TTL;
        resolve(&mut failed, &mut events, None);
        assert!(visible(&events[0]));
        let mut missing =
            resolver(Some(root.path().to_owned()), true, vec![Response::Missing]);
        missing.now += TTL;
        resolve(&mut missing, &mut events, None);
        assert!(!visible(&events[0]));
        let mut cached = resolver(Some(root.path().to_owned()), true, vec![]);
        cached.now += TTL;
        resolve(&mut cached, &mut events, None);
        assert!(!visible(&events[0]));
        assert_eq!(cached.network.calls.len(), 0);
    }

    #[test]
    fn missing_and_undecodable_local_overrides_fall_back_to_github() {
        let root = crate::cache::test_directory().unwrap();
        let path = root.path().join("portrait.png");
        for bytes in [None, Some(b"invalid".as_slice())] {
            if let Some(bytes) = bytes {
                fs::write(&path, bytes).unwrap();
            }
            let mut source = with_avatar(
                "commit@example.invalid",
                Avatar::Local(path.clone(), path.parent().unwrap().to_owned()),
            );
            source.contributor.as_mut().unwrap().accounts.github =
                Some("explicit".to_owned());
            let mut resolver = resolver(None, true, vec![Response::Found(png())]);
            let mut events = [source];
            resolve(&mut resolver, &mut events, None);
            assert!(visible(&events[0]));
            assert_eq!(resolver.network.calls[0].0, EXPLICIT);
        }
    }

    #[cfg(windows)]
    #[test]
    fn local_override_loads_non_unicode_path_offline() {
        use std::os::windows::ffi::OsStringExt;
        let root = crate::cache::test_directory().unwrap();
        let filename = std::ffi::OsString::from_wide(&[0xD800, 0x2E, 0x70, 0x6E, 0x67]);
        let path = root.path().join(filename);
        fs::write(&path, png()).unwrap();
        let mut resolver = resolver(None, false, vec![]);
        let mut events = [with_avatar(
            "commit@example.invalid",
            Avatar::Local(path.clone(), path.parent().unwrap().to_owned()),
        )];
        resolve(&mut resolver, &mut events, None);
        assert!(visible(&events[0]));
        assert_eq!(resolver.network.calls.len(), 0);
    }

    #[test]
    fn parses_both_noreply_formats_and_bots() {
        for (email, expected) in [
            (
                "123+Octocat@users.noreply.github.com",
                Some(("octocat", Some(123))),
            ),
            ("octocat@USERS.NOREPLY.GITHUB.COM", Some(("octocat", None))),
            (
                "42+dependabot[bot]@users.noreply.github.com",
                Some(("dependabot[bot]", Some(42))),
            ),
            ("octocat@example.com", None),
            ("abc+octocat@users.noreply.github.com", None),
            ("+octocat@users.noreply.github.com", None),
            ("123+@users.noreply.github.com", None),
            ("99999999999999999999+a@users.noreply.github.com", None),
            ("@users.noreply.github.com", None),
            ("a/b@users.noreply.github.com", None),
        ] {
            let expected = expected.map(|(login, id)| Account {
                login: login.to_owned(),
                id,
            });
            assert_eq!(noreply(email), expected, "{email}");
        }
    }

    #[test]
    fn github_remote_requires_exact_host_and_repository() {
        for url in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo",
            "https://user@GITHUB.COM/owner/repo.git",
            "git@GITHUB.COM:owner/repo.git",
        ] {
            assert_eq!(github_remote(url).as_deref(), Some("owner/repo"));
        }
        for url in [
            "https://github.com.evil/owner/repo",
            "https://github.com/owner/repo/extra",
            "https://gitlab.com/owner/repo",
            "https://github.com/owner/repo?x=1",
        ] {
            assert!(github_remote(url).is_none());
        }
    }

    #[test]
    fn explicit_github_precedes_noreply_and_commit_api() {
        let mut events = vec![event(
            "123+inferred@users.noreply.github.com",
            Some("explicit"),
        )];
        let mut resolver = resolver(None, true, vec![Response::Found(png())]);
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert!(visible(&events[0]));
        assert_eq!(resolver.network.calls, [(EXPLICIT.to_owned(), false)]);
    }

    #[test]
    fn noreply_uses_the_account_id_without_an_api_request() {
        let mut events = vec![event(
            "49699333+dependabot[bot]@users.noreply.github.com",
            None,
        )];
        let mut resolver = resolver(None, true, vec![Response::Found(png())]);
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert!(visible(&events[0]));
        assert_eq!(
            resolver.network.calls,
            [(
                "https://avatars.githubusercontent.com/u/49699333?s=128".to_owned(),
                false
            )]
        );
    }

    #[test]
    fn commit_api_resolves_once_per_email_and_caches_the_account() {
        let root = crate::cache::test_directory().unwrap();
        let mut events = vec![event("author@example.invalid", None); 3];
        let mut resolver = resolver(
            Some(root.path().to_owned()),
            true,
            vec![commit_by("Linked", 9), Response::Found(png())],
        );
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert!(events.iter().all(visible));
        assert_eq!(resolver.network.calls.len(), 2);
        assert!(resolver.network.calls[0].1);
        assert_eq!(
            resolver.found()["author@example.invalid"],
            Account {
                login: "linked".to_owned(),
                id: Some(9)
            }
        );
        for (fetch, remote) in [(false, None), (true, Some("other/repo"))] {
            let mut cached =
                super::tests::resolver(Some(root.path().to_owned()), fetch, vec![]);
            let mut events = [event("author@example.invalid", None)];
            resolve(&mut cached, &mut events, remote);
            assert!(visible(&events[0]));
            assert_eq!(cached.network.calls.len(), 0);
        }
    }

    #[test]
    fn only_an_unlinked_author_of_the_same_commit_is_a_cached_miss() {
        for (reply, cached) in [
            (
                r#"[{"sha":"ffffffffffffffffffffffffffffffffffffffff","author":{"login":"other","id":1}}]"#,
                false,
            ),
            (
                r#"[{"sha":"0123456789012345678901234567890123456789","author":{"login":"no/slash","id":1}}]"#,
                false,
            ),
            ("[]", false),
            (
                r#"[{"sha":"0123456789012345678901234567890123456789","author":null}]"#,
                true,
            ),
        ] {
            let root = crate::cache::test_directory().unwrap();
            let mut first = resolver(
                Some(root.path().to_owned()),
                true,
                vec![Response::Found(reply.as_bytes().to_vec())],
            );
            let mut events = [event("author@example.invalid", None)];
            resolve(&mut first, &mut events, Some("owner/repo"));
            assert!(first.found().is_empty(), "{reply}");
            assert!(!visible(&events[0]));
            let mut second = resolver(
                Some(root.path().to_owned()),
                true,
                vec![commit_by("linked", 4), Response::Found(png())],
            );
            resolve(&mut second, &mut events, Some("owner/repo"));
            assert_eq!(second.network.calls.is_empty(), cached, "{reply}");
        }
    }

    #[test]
    fn a_cached_miss_holds_only_for_the_remote_that_was_asked() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", None);
        let unlinked = || {
            Response::Found(
                format!(r#"[{{"sha":"{COMMIT}","author":null}}]"#).into_bytes(),
            )
        };
        let mut initial =
            resolver(Some(root.path().to_owned()), true, vec![unlinked()]);
        resolve(
            &mut initial,
            &mut [source.clone(), source.clone()],
            Some("owner/repo"),
        );
        assert_eq!(initial.network.calls.len(), 1);
        let mut same = resolver(Some(root.path().to_owned()), true, vec![]);
        resolve(&mut same, &mut [source.clone()], Some("owner/repo"));
        assert_eq!(same.network.calls.len(), 0);
        let mut other = resolver(Some(root.path().to_owned()), true, vec![unlinked()]);
        resolve(&mut other, &mut [source.clone()], Some("other/repo"));
        assert_eq!(other.network.calls.len(), 1);
        let mut both = resolver(Some(root.path().to_owned()), true, vec![]);
        both.accounts([&source], Some("owner/repo"));
        both.accounts([&source], Some("other/repo"));
        assert_eq!(both.network.calls.len(), 0);
    }

    #[test]
    fn a_cached_login_that_is_not_a_login_is_ignored() {
        let root = crate::cache::test_directory().unwrap();
        let resolver = resolver(Some(root.path().to_owned()), false, vec![]);
        resolver.store(
            "account\nauthor@example.invalid",
            &Lookup {
                checked: 100,
                account: Some(Account {
                    login: "x?email=author@example.invalid&".to_owned(),
                    id: None,
                }),
            },
        );
        let mut offline =
            super::tests::resolver(Some(root.path().to_owned()), false, vec![]);
        offline.accounts([&event("author@example.invalid", None)], None);
        assert!(offline.found().is_empty());
    }

    #[test]
    fn a_confirmed_miss_clears_the_cached_account() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", None);
        let mut found = resolver(
            Some(root.path().to_owned()),
            true,
            vec![commit_by("linked", 7), Response::Found(png())],
        );
        resolve(&mut found, &mut [source.clone()], Some("owner/repo"));
        let unlinked = Response::Found(
            format!(r#"[{{"sha":"{COMMIT}","author":null}}]"#).into_bytes(),
        );
        let mut refresh = resolver(Some(root.path().to_owned()), true, vec![unlinked]);
        refresh.now += TTL;
        resolve(&mut refresh, &mut [source.clone()], Some("owner/repo"));
        let mut offline = resolver(Some(root.path().to_owned()), false, vec![]);
        offline.accounts([&source], Some("owner/repo"));
        assert!(offline.found().is_empty());
    }

    #[test]
    fn a_commit_github_lacks_is_retried_next_run() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", None);
        for reply in [Response::Missing, commit_by("found", 3)] {
            let mut run = resolver(
                Some(root.path().to_owned()),
                true,
                vec![reply, Response::Found(png())],
            );
            resolve(&mut run, &mut [source.clone()], Some("owner/repo"));
            assert!(run.network.calls[0].1);
        }
    }

    #[test]
    fn rate_limit_stops_api_lookups_and_images_still_load() {
        let mut events = vec![
            event("one@example.invalid", None),
            event("two@example.invalid", None),
            event("three@example.invalid", Some("explicit")),
        ];
        let mut resolver = resolver(
            None,
            true,
            vec![Response::RateLimited, Response::Found(png())],
        );
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert!(!visible(&events[0]) && !visible(&events[1]) && visible(&events[2]));
        assert_eq!(
            resolver.network.calls.iter().filter(|call| call.1).count(),
            1
        );
        assert_eq!(resolver.unresolved.len(), 2);
    }

    #[test]
    fn account_lookups_leave_the_image_reserve() {
        let mut resolver = resolver(None, true, vec![]);
        resolver.network.remaining = IMAGE_RESERVE;
        let mut events = [event("author@example.invalid", None)];
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert_eq!(resolver.network.calls.len(), 0);
        assert_eq!(resolver.unresolved.len(), 1);
    }

    #[test]
    fn busiest_emails_are_looked_up_first() {
        let mut events = [
            event("rare@example.invalid", None),
            event("busy@example.invalid", None),
            event("busy@example.invalid", None),
        ];
        events[1].commit = "f".repeat(40);
        let mut resolver = resolver(None, true, vec![Response::RateLimited]);
        resolver.accounts(events.iter(), Some("owner/repo"));
        assert_eq!(resolver.network.calls.len(), 1);
        assert!(resolver.network.calls[0].0.contains(&"f".repeat(40)));
    }

    #[test]
    fn one_contributor_takes_the_best_account_of_its_emails() {
        let mut events = vec![
            event("old@example.invalid", None),
            event("old@users.noreply.github.com", None),
            event("123+inferred@users.noreply.github.com", None),
        ];
        for event in &mut events {
            event.contributor.as_mut().unwrap().key = Key::Config(0);
        }
        let mut resolver =
            resolver(None, true, vec![Response::Missing, Response::Found(png())]);
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert!(events.iter().all(visible));
        assert_eq!(
            resolver.network.calls[1],
            (
                "https://avatars.githubusercontent.com/u/123?s=128".to_owned(),
                false
            )
        );
    }

    #[test]
    fn offline_miss_does_not_call_network() {
        let mut events = vec![event("author@example.invalid", Some("explicit"))];
        let mut resolver = resolver(None, false, vec![]);
        resolve(&mut resolver, &mut events, Some("owner/repo"));
        assert!(!visible(&events[0]));
        assert_eq!(resolver.network.calls.len(), 0);
    }

    #[test]
    fn cache_hit_expiry_and_stale_offline_fallback() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        resolve(&mut initial, &mut [source.clone()], None);
        for (fetch, now) in [(true, 100 + TTL - 1), (false, 100 + TTL + 1)] {
            let mut cached = resolver(Some(root.path().to_owned()), fetch, vec![]);
            cached.now = now;
            let mut events = vec![source.clone()];
            resolve(&mut cached, &mut events, None);
            assert!(visible(&events[0]));
            assert_eq!(cached.network.calls.len(), 0);
        }
        let mut expired = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Unavailable],
        );
        expired.now = 100 + TTL;
        let mut events = vec![source];
        resolve(&mut expired, &mut events, None);
        assert!(visible(&events[0]));
        assert_eq!(expired.network.calls.len(), 1);
    }

    #[test]
    fn expired_miss_retries_and_authoritative_removal_clears_portrait() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let mut missing =
            resolver(Some(root.path().to_owned()), true, vec![Response::Missing]);
        resolve(&mut missing, &mut [source.clone()], None);
        let mut renewed = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        renewed.now += TTL;
        let mut events = vec![source.clone()];
        resolve(&mut renewed, &mut events, None);
        assert!(visible(&events[0]));
        let mut removed =
            resolver(Some(root.path().to_owned()), true, vec![Response::Missing]);
        removed.now += 2 * TTL;
        resolve(&mut removed, &mut events, None);
        assert!(!visible(&events[0]));
        let mut offline = resolver(Some(root.path().to_owned()), false, vec![]);
        resolve(&mut offline, &mut events, None);
        assert!(!visible(&events[0]));
    }

    #[test]
    fn undecodable_refresh_keeps_stale_portrait_offline_and_retries() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let key = format!("image\n{EXPLICIT}");
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        let mut events = [source.clone()];
        resolve(&mut initial, &mut events, None);
        let original_portrait =
            events[0].contributor.as_ref().unwrap().portrait.clone();
        let path = initial.path(&key).unwrap();
        let original_record = fs::read(&path).unwrap();
        let original_png = fs::read(path.with_extension("png")).unwrap();
        let mut expired = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(b"invalid".to_vec())],
        );
        expired.now += TTL;
        resolve(&mut expired, &mut events, None);
        assert_eq!(
            events[0].contributor.as_ref().unwrap().portrait,
            original_portrait
        );
        assert_eq!(fs::read(&path).unwrap(), original_record);
        assert_eq!(fs::read(path.with_extension("png")).unwrap(), original_png);
        let mut retry = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        retry.now += TTL + 1;
        resolve(&mut retry, &mut [source], None);
        assert_eq!(retry.network.calls.len(), 1);
        assert_eq!(retry.read::<Image>(&key).unwrap().checked, retry.now);
    }

    #[test]
    fn first_undecodable_success_is_a_cached_terminal_miss() {
        let root = crate::cache::test_directory().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(b"not an image".to_vec())],
        );
        resolve(&mut initial, &mut [source.clone()], None);
        let mut cached = resolver(Some(root.path().to_owned()), true, vec![]);
        let mut events = [source];
        resolve(&mut cached, &mut events, None);
        assert!(!visible(&events[0]));
        assert_eq!(cached.network.calls.len(), 0);
    }

    #[test]
    fn gif_is_decoded_and_cache_contains_small_png_beside_metadata() {
        let root = crate::cache::test_directory().unwrap();
        let mut gif = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(256, 256, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut gif, image::ImageFormat::Gif)
            .unwrap();
        let mut resolver = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(gif.into_inner())],
        );
        let mut events = [event("author@example.invalid", Some("explicit"))];
        resolve(&mut resolver, &mut events, None);
        assert!(visible(&events[0]));
        let path = resolver.path(&format!("image\n{EXPLICIT}")).unwrap();
        assert!(fs::metadata(&path).unwrap().len() < 4096);
        let png = fs::read(path.with_extension("png")).unwrap();
        assert!(u64::try_from(png.len()).unwrap() < CACHE_BYTES);
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (128, 128));
    }

    #[test]
    fn http_maps_statuses_and_caps_response_bodies() {
        for status in [401, 403, 429] {
            assert!(matches!(
                response_body(status, Cursor::new([])),
                Response::RateLimited
            ));
        }
        for status in [404, 422] {
            assert!(matches!(
                response_body(status, Cursor::new([])),
                Response::Missing
            ));
        }
        assert!(matches!(
            response_body(500, Cursor::new([])),
            Response::Unavailable
        ));
        assert!(
            matches!(response_body(200, Cursor::new(b"ok")), Response::Found(bytes) if bytes == b"ok")
        );
        let bytes = vec![0; usize::try_from(MAX_BYTES).unwrap()];
        assert!(matches!(
            response_body(200, Cursor::new(&bytes)),
            Response::Found(_)
        ));
        assert!(matches!(
            response_body(200, Cursor::new(vec![0; bytes.len() + 1])),
            Response::Unavailable
        ));
        assert!(matches!(response_body(200, Broken), Response::Unavailable));
    }

    #[test]
    #[expect(
        clippy::disallowed_types,
        clippy::disallowed_methods,
        reason = "exercise the avatar HTTP client against a local test server"
    )]
    fn http_get_uses_status_mapping_with_local_server() {
        use std::{net::TcpListener, thread};

        for status in [200, 401, 403, 404, 422, 429, 500] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || serve_once(&listener, status));
            let mut http = Http::new();
            http.client = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .proxy(None)
                .timeout_global(Some(Duration::from_secs(2)))
                .build()
                .into();
            let response = http.get(&format!("http://{address}/"), true);
            server.join().unwrap();
            assert!(match status {
                200 => matches!(response, Response::Found(bytes) if bytes == b"ok"),
                401 | 403 | 429 => matches!(response, Response::RateLimited),
                404 | 422 => matches!(response, Response::Missing),
                _ => matches!(response, Response::Unavailable),
            });
        }
    }

    #[test]
    fn http_disables_failed_hosts_and_obeys_overall_deadline() {
        let mut http = Http::new();
        assert!(matches!(
            http.get("http://127.0.0.1/", false),
            Response::Unavailable
        ));
        assert_eq!(http.failures, 1);
        assert!(matches!(
            http.get("http://127.0.0.1/", false),
            Response::Unavailable
        ));
        assert_eq!(http.skipped, 1);
        http.deadline = Instant::now();
        assert!(matches!(
            http.get("https://example.invalid/", false),
            Response::Unavailable
        ));
        assert_eq!(http.skipped, 2);
        assert_eq!(http.failures, 1);
    }
}
