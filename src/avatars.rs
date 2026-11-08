use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{contributors::Key, timeline::Event};

const TTL: u64 = 7 * 24 * 60 * 60;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const CACHE_BYTES: u64 = 128 * 1024;
const FETCH_BUDGET: Duration = Duration::from_secs(30);

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

#[derive(Serialize, Deserialize)]
struct Record {
    checked: u64,
    github: Option<String>,
    image: bool,
}

#[derive(Deserialize)]
struct Account {
    login: String,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct CommitAccount {
    author: Option<Account>,
}

pub(super) enum Response {
    Found(Vec<u8>),
    Missing,
    Unavailable,
    RateLimited,
}

pub(super) trait Network {
    fn get(&mut self, url: &str, github_api: bool) -> Response;

    fn summary(&self, _portraits: usize, _contributors: usize, _unsupported: bool) {}
}

pub(super) struct Http {
    client: ureq::Agent,
    token: Option<String>,
    deadline: Instant,
    dead: HashSet<String>,
    failures: usize,
    limited: usize,
    skipped: usize,
}

impl Http {
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
        let remaining = self.deadline.saturating_duration_since(Instant::now());
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

    fn summary(&self, portraits: usize, contributors: usize, unsupported: bool) {
        eprintln!(
            "avatars: contributors={contributors} portraits={portraits} failures={} limited={} skipped={} unsupported_remotes={}",
            self.failures,
            self.limited,
            self.skipped,
            usize::from(unsupported)
        );
    }
}

impl Network for Option<Http> {
    fn get(&mut self, url: &str, github_api: bool) -> Response {
        self.as_mut()
            .map_or(Response::Unavailable, |http| http.get(url, github_api))
    }

    fn summary(&self, portraits: usize, contributors: usize, unsupported: bool) {
        if let Some(http) = self {
            http.summary(portraits, contributors, unsupported);
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

fn hash(value: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    Sha256::digest(value)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
}

pub(super) fn noreply(email: &str) -> Option<String> {
    let (local, host) = email.rsplit_once('@')?;
    if !host.eq_ignore_ascii_case("users.noreply.github.com") {
        return None;
    }
    let login = if let Some((id, login)) = local.split_once('+') {
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        login
    } else {
        local
    };
    valid_login(login).then(|| login.to_ascii_lowercase())
}

fn valid_login(login: &str) -> bool {
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

fn priority(event: &Event) -> u8 {
    if event
        .contributor
        .as_ref()
        .is_some_and(|contributor| contributor.accounts.github.is_some())
    {
        0
    } else if noreply(&String::from_utf8_lossy(&event.author.email)).is_some() {
        1
    } else {
        2
    }
}

fn github_image(
    login: &str,
    email: &str,
    explicit: bool,
    avatar_url: Option<String>,
) -> String {
    if let Some(url) = avatar_url {
        return format!("{}{}s=128", url, if url.contains('?') { "&" } else { "?" });
    }
    if !explicit
        && noreply(email).is_some()
        && let Some((id, _)) = email.split_once('+')
    {
        return format!("https://avatars.githubusercontent.com/u/{id}?s=128");
    }
    let encoded = login.replace('[', "%5B").replace(']', "%5D");
    format!("https://github.com/{encoded}.png?size=128")
}

pub(super) struct Resolver<N> {
    pub network: N,
    pub root: Option<PathBuf>,
    pub fetch: bool,
    pub now: u64,
    limited: bool,
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
            memo: HashMap::new(),
        }
    }

    pub(super) fn resolve(&mut self, events: &mut [Event], remote: Option<&str>) {
        let mut portraits = HashMap::<Key, Arc<Portrait>>::new();
        let mut seen = HashSet::new();
        let mut candidates: Vec<_> = events
            .iter_mut()
            .filter(|event| {
                event.contributor.as_ref().is_some_and(|contributor| {
                    seen.insert((
                        contributor.key.clone(),
                        event.author.email.to_ascii_lowercase(),
                    ))
                })
            })
            .collect();
        candidates.sort_by_key(|event| priority(event));
        let total = candidates.len();
        let mut processed = 0;
        for event in candidates {
            let Some(contributor) = &mut event.contributor else {
                continue;
            };
            if portraits.contains_key(&contributor.key) {
                continue;
            }
            let email = String::from_utf8_lossy(&event.author.email)
                .trim()
                .to_ascii_lowercase();
            let accounts = &contributor.accounts;
            let key = format!(
                "v1\n{email}\n{:?}\n{:?}\n{remote:?}",
                accounts.github, accounts.gravatar
            );
            let portrait = if let Some(portrait) = self.memo.get(&key) {
                portrait.clone()
            } else {
                processed += 1;
                if self.fetch {
                    eprintln!(
                        "avatars: resolving {processed}/{total} (30s fetch budget)"
                    );
                }
                let portrait = self.one(
                    &key,
                    &email,
                    accounts.github.as_deref(),
                    accounts.gravatar.as_deref(),
                    remote,
                    &event.commit,
                );
                self.memo.insert(key, portrait.clone());
                portrait
            };
            if let Some(portrait) = portrait {
                portraits.insert(contributor.key.clone(), portrait);
            }
        }
        self.network
            .summary(portraits.len(), seen.len(), remote.is_none());
        for event in events {
            if let Some(contributor) = &mut event.contributor {
                contributor.portrait = portraits.get(&contributor.key).cloned();
            }
        }
    }

    fn read(&self, key: &str) -> Option<Record> {
        let path = self.path(key)?;
        let file = fs::File::open(path).ok()?;
        serde_json::from_reader(file.take(4096)).ok()
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        Some(
            self.root
                .as_ref()?
                .join("avatars-v2")
                .join(format!("{}.json", hash(key))),
        )
    }

    fn store(&self, key: &str, record: &Record, image: Option<&[u8]>) -> Option<()> {
        let path = self.path(key)?;
        let parent = path.parent()?;
        fs::create_dir_all(parent).ok()?;
        if let Some(image) = image {
            let mut file = tempfile::NamedTempFile::new_in(parent).ok()?;
            file.write_all(image).ok()?;
            file.flush().ok()?;
            file.persist(path.with_extension("png")).ok()?;
        }
        let mut file = tempfile::NamedTempFile::new_in(parent).ok()?;
        serde_json::to_writer(file.as_file_mut(), record).ok()?;
        file.flush().ok()?;
        file.persist(path).ok()?;
        Some(())
    }

    fn portrait(&self, key: &str) -> Option<Arc<Portrait>> {
        let path = self.path(key)?.with_extension("png");
        let file = fs::File::open(path).ok()?;
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

    fn one(
        &mut self,
        key: &str,
        email: &str,
        github: Option<&str>,
        gravatar: Option<&str>,
        remote: Option<&str>,
        commit: &str,
    ) -> Option<Arc<Portrait>> {
        let cached = self.read(key);
        let stale = cached
            .as_ref()
            .filter(|record| record.image)
            .and_then(|_| self.portrait(key));
        if !self.fetch
            || cached.as_ref().is_some_and(|record| {
                self.now.saturating_sub(record.checked) < TTL
                    && (!record.image || stale.is_some())
            })
        {
            return stale;
        }
        let mut complete = true;
        let mut login = github
            .filter(|login| valid_login(login))
            .map(str::to_owned)
            .or_else(|| noreply(email))
            .or_else(|| {
                cached
                    .as_ref()
                    .and_then(|record| record.github.clone())
                    .filter(|login| valid_login(login))
            });
        let mut avatar_url = None;
        if login.is_none()
            && let Some(remote) = remote
            && !commit.is_empty()
            && let Some(account) = self.account(remote, commit, &mut complete)
        {
            login = Some(account.login);
            avatar_url = account.avatar_url;
        }
        let mut image = None;
        if let Some(login) = &login {
            let url = github_image(login, email, github.is_some(), avatar_url);
            image = self.image(&url, &mut complete, stale.is_some());
        }
        let email = gravatar.unwrap_or(email).trim().to_ascii_lowercase();
        if image.is_none() && !email.is_empty() {
            image = self.image(
                &format!(
                    "https://www.gravatar.com/avatar/{}?s=128&d=404",
                    hash(email)
                ),
                &mut complete,
                stale.is_some(),
            );
        }
        let portrait = image.as_deref().and_then(Portrait::decode).map(Arc::new);
        if complete {
            let record = Record {
                checked: self.now,
                github: login,
                image: image.is_some(),
            };
            let _ = self.store(key, &record, image.as_deref());
            portrait
        } else {
            stale.or(portrait)
        }
    }

    fn account(
        &mut self,
        remote: &str,
        commit: &str,
        complete: &mut bool,
    ) -> Option<Account> {
        if self.limited {
            *complete = false;
            return None;
        }
        match self.network.get(
            &format!(
                "https://api.github.com/repos/{remote}/commits?sha={commit}&per_page=1"
            ),
            true,
        ) {
            Response::Found(bytes) => {
                let Ok(response) = serde_json::from_slice::<Vec<CommitAccount>>(&bytes)
                else {
                    *complete = false;
                    return None;
                };
                response
                    .into_iter()
                    .next()
                    .and_then(|commit| commit.author)
                    .filter(|account| valid_login(&account.login))
                    .map(|mut account| {
                        account.avatar_url = account.avatar_url.filter(|url| {
                            url.starts_with("https://avatars.githubusercontent.com/")
                        });
                        account
                    })
            }
            Response::Missing => None,
            Response::RateLimited => {
                self.limited = true;
                *complete = false;
                None
            }
            Response::Unavailable => {
                *complete = false;
                None
            }
        }
    }
    fn image(
        &mut self,
        url: &str,
        complete: &mut bool,
        has_stale: bool,
    ) -> Option<Vec<u8>> {
        match self.network.get(url, false) {
            Response::Found(bytes) => {
                let encoded = (|| {
                    let portrait = Portrait::decode(&bytes)?;
                    let image = image::RgbaImage::from_raw(128, 128, portrait.pixels)?;
                    let mut png = Cursor::new(Vec::new());
                    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
                    Some(png.into_inner())
                })();
                if encoded.is_none() && has_stale {
                    *complete = false;
                }
                encoded
            }
            Response::Missing => None,
            Response::Unavailable | Response::RateLimited => {
                *complete = false;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::contributors::{Accounts, Contributor};

    #[test]
    fn hash_is_lowercase_sha256_hex() {
        assert_eq!(
            hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[derive(Default)]
    struct Fake {
        replies: VecDeque<Response>,
        calls: Vec<(String, bool)>,
    }

    impl Network for Fake {
        fn get(&mut self, url: &str, api: bool) -> Response {
            self.calls.push((url.to_owned(), api));
            self.replies
                .pop_front()
                .expect("each network call has a scripted response")
        }
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("transport failed"))
        }
    }

    fn serve_once(listener: &std::net::TcpListener, status: u16) {
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
            "HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
        )
        .unwrap();
    }

    fn png() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn event(email: &str, github: Option<&str>) -> Event {
        Event {
            author: crate::timeline::Identity {
                name: b"Author".to_vec(),
                email: email.as_bytes().to_vec(),
            },
            contributor: Some(Contributor {
                key: Key::Email(email.as_bytes().to_vec()),
                name: "Author".to_owned(),
                accounts: Accounts {
                    github: github.map(str::to_owned),
                    gravatar: None,
                },
                portrait: None,
            }),
            commit: "0123456789012345678901234567890123456789".to_owned(),
            seconds: 0,
            changes: vec![],
        }
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
            },
            root,
            fetch,
            100,
        )
    }

    fn visible(event: &Event) -> bool {
        event.contributor.as_ref().unwrap().portrait.is_some()
    }

    #[test]
    fn parses_both_noreply_formats_and_bots() {
        for (email, expected) in [
            ("123+Octocat@users.noreply.github.com", Some("octocat")),
            ("octocat@USERS.NOREPLY.GITHUB.COM", Some("octocat")),
            (
                "42+dependabot[bot]@users.noreply.github.com",
                Some("dependabot[bot]"),
            ),
            ("octocat@example.com", None),
            ("abc+octocat@users.noreply.github.com", None),
            ("+octocat@users.noreply.github.com", None),
            ("123+@users.noreply.github.com", None),
            ("@users.noreply.github.com", None),
            ("a/b@users.noreply.github.com", None),
        ] {
            assert_eq!(noreply(email).as_deref(), expected, "{email}");
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
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(visible(&events[0]));
        assert_eq!(
            resolver.network.calls,
            [("https://github.com/explicit.png?size=128".to_owned(), false)]
        );
    }

    #[test]
    fn noreply_precedes_commit_api() {
        let mut events = vec![event("123+inferred@users.noreply.github.com", None)];
        let mut resolver = resolver(None, true, vec![Response::Found(png())]);
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(visible(&events[0]));
        assert!(resolver.network.calls[0].0.contains("/u/123?s=128"));
        assert_eq!(resolver.network.calls.len(), 1);
    }

    #[test]
    fn commit_api_resolves_once_per_email_and_caches_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut events = vec![event("author@example.invalid", None); 3];
        let mut resolver = resolver(
            Some(root.path().to_owned()),
            true,
            vec![
                Response::Found(br#"[{"author":{"login":"linked"}}]"#.to_vec()),
                Response::Found(png()),
            ],
        );
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(events.iter().all(visible));
        assert_eq!(resolver.network.calls.len(), 2);
        assert!(resolver.network.calls[0].1);
        let entry = fs::read_dir(root.path().join("avatars-v2"))
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .unwrap();
        let record: Record =
            serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
        assert_eq!(record.github.as_deref(), Some("linked"));
        assert!(resolver.network.calls[0].0.ends_with("&per_page=1"));
        let mut refreshed = self::resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        refreshed.now = 100 + TTL;
        refreshed.resolve(&mut events, Some("owner/repo"));
        assert_eq!(refreshed.network.calls.len(), 1);
        assert!(!refreshed.network.calls[0].1);
        assert!(refreshed.network.calls[0].0.contains("/linked.png"));
    }

    #[test]
    fn unlinked_author_falls_back_to_gravatar_without_placeholder() {
        let mut events = vec![event(" MyEmailAddress@example.com ", None)];
        let mut resolver = resolver(
            None,
            true,
            vec![
                Response::Found(br#"[{"author":null}]"#.to_vec()),
                Response::Missing,
            ],
        );
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(!visible(&events[0]));
        assert!(
            resolver.network.calls[1]
                .0
                .contains("84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee?s=128&d=404")
        );
    }

    #[test]
    fn explicit_gravatar_email_is_used_for_fallback() {
        let mut events = vec![event("unrelated@example.invalid", None)];
        events[0].contributor.as_mut().unwrap().accounts.gravatar =
            Some("MyEmailAddress@example.com".to_owned());
        let mut resolver = resolver(None, true, vec![Response::Found(png())]);
        resolver.resolve(&mut events, None);
        assert!(visible(&events[0]));
        assert!(resolver.network.calls[0].0.contains(
            "84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee"
        ));
    }

    #[test]
    fn offline_miss_does_not_call_network() {
        let mut events = vec![event("author@example.invalid", Some("explicit"))];
        let mut resolver = resolver(None, false, vec![]);
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(!visible(&events[0]));
        assert_eq!(resolver.network.calls.len(), 0);
    }

    #[test]
    fn cache_hit_expiry_and_stale_offline_fallback() {
        let root = tempfile::tempdir().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        initial.resolve(&mut [source.clone()], None);
        for (fetch, now) in [(true, 100 + TTL - 1), (false, 100 + TTL + 1)] {
            let mut cached = resolver(Some(root.path().to_owned()), fetch, vec![]);
            cached.now = now;
            let mut events = vec![source.clone()];
            cached.resolve(&mut events, None);
            assert!(visible(&events[0]));
            assert_eq!(cached.network.calls.len(), 0);
        }
        let mut expired = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Unavailable, Response::Unavailable],
        );
        expired.now = 100 + TTL;
        let mut events = vec![source];
        expired.resolve(&mut events, None);
        assert!(visible(&events[0]));
        assert_eq!(expired.network.calls.len(), 2);
    }

    #[test]
    fn negative_cache_and_duplicate_emails_avoid_requests() {
        let root = tempfile::tempdir().unwrap();
        let source = event("author@example.invalid", None);
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Missing, Response::Missing],
        );
        initial.resolve(&mut [source.clone(), source.clone()], Some("owner/repo"));
        assert_eq!(initial.network.calls.len(), 2);
        let mut cached = resolver(Some(root.path().to_owned()), true, vec![]);
        cached.resolve(&mut [source], Some("owner/repo"));
        assert_eq!(cached.network.calls.len(), 0);
    }

    #[test]
    fn rate_limit_stops_subsequent_api_calls_and_keeps_fallback() {
        let mut events = vec![
            event("one@example.invalid", None),
            event("two@example.invalid", None),
        ];
        let mut resolver = resolver(
            None,
            true,
            vec![
                Response::RateLimited,
                Response::Missing,
                Response::Found(png()),
            ],
        );
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(!visible(&events[0]));
        assert!(visible(&events[1]));
        assert_eq!(
            resolver.network.calls.iter().filter(|call| call.1).count(),
            1
        );
    }

    #[test]
    fn corrupt_image_uses_sphere_and_aliases_share_resolved_portrait() {
        let mut events = vec![
            event("one@example.invalid", Some("one")),
            event("two@example.invalid", Some("one")),
        ];
        for event in &mut events {
            event.contributor.as_mut().unwrap().key = Key::Config(0);
        }
        let mut resolver = resolver(
            None,
            true,
            vec![
                Response::Found(b"invalid".to_vec()),
                Response::Missing,
                Response::Found(png()),
            ],
        );
        resolver.resolve(&mut events, None);
        assert!(events.iter().all(visible));
        assert_eq!(resolver.network.calls.len(), 3);
    }
    #[test]
    fn expired_miss_retries_and_authoritative_removal_clears_portrait() {
        let root = tempfile::tempdir().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let mut missing = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Missing, Response::Missing],
        );
        missing.resolve(&mut [source.clone()], None);
        let mut renewed = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(png())],
        );
        renewed.now += TTL;
        let mut events = vec![source.clone()];
        renewed.resolve(&mut events, None);
        assert!(visible(&events[0]));
        let mut removed = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Missing, Response::Missing],
        );
        removed.now += 2 * TTL;
        removed.resolve(&mut events, None);
        assert!(!visible(&events[0]));
        let mut offline = resolver(Some(root.path().to_owned()), false, vec![]);
        offline.resolve(&mut events, None);
        assert!(!visible(&events[0]));
    }

    #[test]
    fn numeric_bot_noreply_uses_avatar_id_without_api_request() {
        let mut events = vec![event(
            "49699333+dependabot[bot]@users.noreply.github.com",
            None,
        )];
        let mut resolver = resolver(None, true, vec![Response::Found(png())]);
        resolver.resolve(&mut events, Some("owner/repo"));
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
    fn noreply_alias_precedes_an_earlier_unresolved_email() {
        let mut events = vec![
            event("old@example.invalid", None),
            event("123+inferred@users.noreply.github.com", None),
        ];
        for event in &mut events {
            event.contributor.as_mut().unwrap().key = Key::Config(0);
        }
        let mut resolver = resolver(None, true, vec![Response::Found(png())]);
        resolver.resolve(&mut events, Some("owner/repo"));
        assert!(events.iter().all(visible));
        assert_eq!(
            resolver.network.calls,
            [(
                "https://avatars.githubusercontent.com/u/123?s=128".to_owned(),
                false
            )]
        );
    }

    #[test]
    fn incomplete_refresh_does_not_replace_cached_github_portrait() {
        let root = tempfile::tempdir().unwrap();
        let source = event("author@example.invalid", None);
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![
                Response::Found(br#"[{"author":{"login":"linked"}}]"#.to_vec()),
                Response::Found(png()),
            ],
        );
        initial.resolve(&mut [source.clone()], Some("owner/repo"));
        let key = initial.memo.keys().next().unwrap().clone();
        let original = fs::read(initial.path(&key).unwrap()).unwrap();
        let mut partial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Unavailable, Response::Found(png())],
        );
        partial.now += TTL;
        let mut events = [source.clone()];
        partial.resolve(&mut events, Some("owner/repo"));
        assert!(visible(&events[0]));
        assert_eq!(fs::read(partial.path(&key).unwrap()).unwrap(), original);
        let mut offline = resolver(Some(root.path().to_owned()), false, vec![]);
        offline.resolve(&mut [source], Some("owner/repo"));
        assert_eq!(
            offline.read(&key).unwrap().github.as_deref(),
            Some("linked")
        );
        assert_eq!(offline.read(&key).unwrap().checked, 100);
    }

    #[test]
    fn undecodable_refresh_keeps_stale_portrait_offline_and_retries() {
        for replies in [
            vec![Response::Found(b"invalid".to_vec()), Response::Missing],
            vec![Response::Missing, Response::Found(b"invalid".to_vec())],
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = event("author@example.invalid", Some("explicit"));
            let mut initial = resolver(
                Some(root.path().to_owned()),
                true,
                vec![Response::Found(png())],
            );
            let mut events = [source.clone()];
            initial.resolve(&mut events, None);
            let original_portrait =
                events[0].contributor.as_ref().unwrap().portrait.clone();
            let key = initial.memo.keys().next().unwrap().clone();
            let path = initial.path(&key).unwrap();
            let original_record = fs::read(&path).unwrap();
            let original_png = fs::read(path.with_extension("png")).unwrap();
            let mut expired = resolver(Some(root.path().to_owned()), true, replies);
            expired.now += TTL;
            expired.resolve(&mut events, None);
            assert_eq!(
                events[0].contributor.as_ref().unwrap().portrait,
                original_portrait
            );
            assert_eq!(fs::read(&path).unwrap(), original_record);
            assert_eq!(fs::read(path.with_extension("png")).unwrap(), original_png);
            let mut offline = resolver(Some(root.path().to_owned()), false, vec![]);
            offline.now += TTL;
            let mut events = [source.clone()];
            offline.resolve(&mut events, None);
            assert_eq!(
                events[0].contributor.as_ref().unwrap().portrait,
                original_portrait
            );
            let mut retry = resolver(
                Some(root.path().to_owned()),
                true,
                vec![Response::Found(png())],
            );
            retry.now += TTL + 1;
            retry.resolve(&mut [source], None);
            assert_eq!(retry.network.calls.len(), 1);
            assert_eq!(retry.read(&key).unwrap().checked, retry.now);
        }
    }
    #[test]
    fn first_undecodable_success_is_a_cached_terminal_miss() {
        let root = tempfile::tempdir().unwrap();
        let source = event("author@example.invalid", Some("explicit"));
        let mut initial = resolver(
            Some(root.path().to_owned()),
            true,
            vec![Response::Found(b"not an image".to_vec()), Response::Missing],
        );
        initial.resolve(&mut [source.clone()], None);
        let mut cached = resolver(Some(root.path().to_owned()), true, vec![]);
        let mut events = [source];
        cached.resolve(&mut events, None);
        assert!(!visible(&events[0]));
        assert_eq!(cached.network.calls.len(), 0);
    }

    #[test]
    fn gif_is_decoded_and_cache_contains_small_png_beside_metadata() {
        let root = tempfile::tempdir().unwrap();
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
        resolver.resolve(&mut events, None);
        assert!(visible(&events[0]));
        let key = resolver.memo.keys().next().unwrap();
        let path = resolver.path(key).unwrap();
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
