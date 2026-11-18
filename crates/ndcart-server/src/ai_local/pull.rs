//! `ndcart ai pull` and `ndcart ai import`: bring a model file onto this computer safely.
//!
//! The studio downloads **model files only** (GGUF), never programs. It reads the file's size
//! and SHA-256 from the Hub's tree API, checks free disk, shows the size and asks, then
//! streams the file into `<data>/models/<repo>/<file>.part` (resumable with `Range`), hashes
//! while it writes, and renames only after the hash matches. Redirects are followed by hand,
//! and only to hosts on a short list; the Hub token (`HF_TOKEN` or `HF_TOKEN_FILE`, never an
//! argument) goes only to the Hub's own host.
//!
//! `ND_CARTO_HF_BASE` replaces the Hub address and is a test hook (it also allows that host
//! and plain http); the CDN host names the Hub redirects to are matched by suffix
//! (`huggingface.co`, `hf.co`) because they were not confirmed live when this was written.

use super::catalog::{self, Entry};
use super::{human_bytes, iso_date, now_secs};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const HF_BASE: &str = "https://huggingface.co";
const MAX_HOPS: usize = 6;

/// Where downloads may go.
#[derive(Clone, Debug)]
pub struct Policy {
    pub base: String,
    /// A host is allowed when it equals one of these or is a subdomain of one.
    pub hosts: Vec<String>,
    pub allow_http: bool,
}

impl Policy {
    pub fn hub() -> Policy {
        Policy {
            base: HF_BASE.into(),
            hosts: vec!["huggingface.co".into(), "hf.co".into()],
            allow_http: false,
        }
    }

    /// The Hub, or `ND_CARTO_HF_BASE` (a test hook).
    pub fn from_env() -> Policy {
        let mut p = Policy::hub();
        if let Ok(b) = std::env::var("ND_CARTO_HF_BASE") {
            let b = b.trim().trim_end_matches('/').to_string();
            if let Ok(u) = Url::parse(&b) {
                if let Some(h) = u.host_str() {
                    p.hosts.push(h.to_string());
                    p.allow_http = u.scheme() == "http";
                    p.base = b;
                }
            }
        }
        p
    }

    fn base_host(&self) -> Option<String> {
        Url::parse(&self.base).ok()?.host_str().map(str::to_string)
    }

    pub fn check(&self, url: &Url) -> Result<(), String> {
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            s => return Err(format!("refusing a {s}:// download address ({url})")),
        }
        let host = url.host_str().ok_or("a download address has no host")?;
        let ok = self
            .hosts
            .iter()
            .any(|h| host == h || host.ends_with(&format!(".{h}")));
        if ok {
            Ok(())
        } else {
            Err(format!(
                "refusing to download from {host}: it is not a Hugging Face host"
            ))
        }
    }

    /// The token may go only to the Hub's own host, never to a CDN it redirects to.
    fn token_ok(&self, url: &Url) -> bool {
        url.host_str().map(str::to_string) == self.base_host()
    }
}

/// `HF_TOKEN`, else the first line of the file named by `HF_TOKEN_FILE`.
pub fn token_from_env() -> Option<String> {
    if let Some(t) = std::env::var("HF_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Some(t.trim().to_string());
    }
    let f = std::env::var("HF_TOKEN_FILE")
        .ok()
        .filter(|v| !v.is_empty())?;
    std::fs::read_to_string(f)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

pub fn client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("ndcart/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

// ---- the tree API --------------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
pub struct TreeItem {
    #[serde(rename = "type", default)]
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub lfs: Option<Lfs>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Lfs {
    pub oid: String,
    #[serde(default)]
    pub size: Option<u64>,
}

/// One file to fetch.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FileSpec {
    pub path: String,
    pub size: u64,
    /// The SHA-256 to verify against (the Hub's LFS oid, or a pinned one); None when the Hub
    /// gave none (a file that is not stored with LFS).
    pub sha256: Option<String>,
    /// The checksum came from the catalog, not the Hub.
    #[serde(skip)]
    pub pinned: bool,
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl TreeItem {
    fn spec(&self) -> FileSpec {
        let size = self
            .lfs
            .as_ref()
            .and_then(|l| l.size)
            .or(self.size)
            .unwrap_or(0);
        let sha256 = self
            .lfs
            .as_ref()
            .map(|l| l.oid.to_ascii_lowercase())
            .filter(|o| is_sha256(o));
        FileSpec {
            path: self.path.clone(),
            size,
            sha256,
            pinned: false,
        }
    }
}

/// `name-00001-of-00003.gguf` -> (`name`, 1, 3).
fn split_parts(path: &str) -> Option<(&str, u32, u32)> {
    let stem = path.strip_suffix(".gguf")?;
    let (rest, total) = stem.rsplit_once("-of-")?;
    let (prefix, index) = rest.rsplit_once('-')?;
    if index.len() != 5 || total.len() != 5 {
        return None;
    }
    Some((prefix, index.parse().ok()?, total.parse().ok()?))
}

/// Choose the file (or the whole split set) to fetch from a repo listing: the exact name if
/// given and present, else the one `.gguf` that names the quant. Never a vision projector.
pub fn select(
    tree: &[TreeItem],
    file: Option<&str>,
    quant: Option<&str>,
) -> Result<Vec<FileSpec>, String> {
    let ggufs: Vec<&TreeItem> = tree
        .iter()
        .filter(|t| t.kind != "directory" && t.path.ends_with(".gguf"))
        .collect();
    if ggufs.is_empty() {
        return Err("the repository lists no .gguf files".into());
    }
    let exact = file.and_then(|f| ggufs.iter().find(|t| t.path == f));
    let chosen: &TreeItem = match exact {
        Some(t) => t,
        None => {
            let mut cands: Vec<&TreeItem> = ggufs
                .iter()
                .copied()
                .filter(|t| !t.path.to_ascii_lowercase().contains("mmproj"))
                .filter(|t| match quant {
                    Some(q) => t
                        .path
                        .to_ascii_lowercase()
                        .contains(&q.to_ascii_lowercase()),
                    None => file.is_none(),
                })
                .collect();
            // A split set counts once, through its first part.
            cands.retain(|t| split_parts(&t.path).is_none_or(|(_, i, _)| i == 1));
            match cands.len() {
                1 => cands[0],
                0 => {
                    return Err(format!(
                        "{} is not in the repository. Files there: {}",
                        file.or(quant).unwrap_or("that file"),
                        names(&ggufs)
                    ))
                }
                _ => {
                    return Err(format!(
                        "more than one file matches; name one with repo:File.gguf. Candidates: {}",
                        names(&cands)
                    ))
                }
            }
        }
    };
    let mut out = vec![chosen.spec()];
    if let Some((prefix, 1, total)) = split_parts(&chosen.path) {
        out.clear();
        for i in 1..=total {
            let want = format!("{prefix}-{i:05}-of-{total:05}.gguf");
            let part = ggufs
                .iter()
                .find(|t| t.path == want)
                .ok_or_else(|| format!("the split set is missing part {i} of {total} ({want})"))?;
            out.push(part.spec());
        }
    }
    Ok(out)
}

fn names(v: &[&TreeItem]) -> String {
    let mut n: Vec<&str> = v.iter().map(|t| t.path.as_str()).collect();
    n.truncate(12);
    n.join(", ")
}

// ---- targets and plans ---------------------------------------------------------------------

/// What `pull` was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub repo: String,
    pub file: Option<String>,
    pub quant: Option<String>,
    pub license: String,
    pub catalog_id: Option<String>,
    pub pinned_sha256: Option<String>,
}

fn repo_ok(r: &str) -> bool {
    let mut parts = r.split('/');
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if ok(a) && ok(b))
}

/// A catalog id, or `owner/Repo-GGUF:File.gguf` / `owner/Repo-GGUF:QUANT`.
pub fn parse_target(arg: &str, cat: &[Entry], allow_non_unsloth: bool) -> Result<Target, String> {
    if let Some(e) = catalog::find(cat, arg) {
        return Ok(Target {
            repo: e.hf_repo.clone(),
            file: Some(e.file.clone()),
            quant: Some(e.quant.clone()),
            license: e.license.clone(),
            catalog_id: Some(e.id.clone()),
            pinned_sha256: e.sha256.clone(),
        });
    }
    if !arg.contains('/') {
        return Err(format!(
            "'{arg}' is not a catalog id (see: ndcart ai models) and not a repo like unsloth/Name-GGUF:File.gguf"
        ));
    }
    let (repo, what) = match arg.split_once(':') {
        Some((r, w)) => (r, Some(w)),
        None => (arg, None),
    };
    if !repo_ok(repo) {
        return Err(format!(
            "'{repo}' is not a valid repository name (owner/name)"
        ));
    }
    if !catalog::is_unsloth_repo(repo) && !allow_non_unsloth {
        return Err(format!(
            "{repo} is not an unsloth/<name>-GGUF repository; add --allow-non-unsloth if you trust it"
        ));
    }
    let (file, quant) = match what {
        Some(w) if w.ends_with(".gguf") => (Some(w.to_string()), None),
        Some(w) if !w.is_empty() => (None, Some(w.to_string())),
        _ => (None, None),
    };
    Ok(Target {
        repo: repo.to_string(),
        file,
        quant,
        license: String::new(),
        catalog_id: None,
        pinned_sha256: None,
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct Plan {
    pub repo: String,
    pub files: Vec<FileSpec>,
    pub total_bytes: u64,
    pub license: String,
    pub catalog_id: Option<String>,
}

impl Plan {
    /// The first file's path under `<data>/models/<repo>/` (a split set loads from part 1).
    pub fn model_file(&self) -> String {
        format!("{}/{}", self.repo, self.files[0].path)
    }
}

fn url_path(p: &str) -> String {
    p.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A relative path that stays inside its folder.
fn safe_rel(p: &str) -> Result<(), String> {
    let bad = p.is_empty()
        || p.starts_with('/')
        || p.contains('\\')
        || p.contains(':')
        || p.split('/').any(|s| s.is_empty() || s == "." || s == "..");
    if bad {
        Err(format!("unsafe file name from the repository: {p}"))
    } else {
        Ok(())
    }
}

/// Free bytes under a path (injectable for tests).
pub type FreeFn = Box<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

/// The machine and network the pull runs against (injectable for tests).
pub struct Pull {
    pub client: Client,
    pub policy: Policy,
    pub token: Option<String>,
    pub free_bytes: FreeFn,
}

impl Pull {
    pub fn from_env() -> Pull {
        Pull {
            client: client(),
            policy: Policy::from_env(),
            token: token_from_env(),
            free_bytes: Box::new(free_bytes),
        }
    }

    /// GET with redirects followed by hand, only to allowed hosts, the token only to the
    /// Hub's host, the `Range` header kept.
    async fn get(&self, url: &str, range_from: Option<u64>) -> Result<reqwest::Response, String> {
        let mut url = Url::parse(url).map_err(|e| format!("bad address {url}: {e}"))?;
        for _ in 0..=MAX_HOPS {
            self.policy.check(&url)?;
            let mut req = self.client.get(url.clone());
            if let (Some(t), true) = (&self.token, self.policy.token_ok(&url)) {
                req = req.bearer_auth(t);
            }
            if let Some(from) = range_from {
                req = req.header("Range", format!("bytes={from}-"));
            }
            let resp = req.send().await.map_err(|e| {
                format!(
                    "cannot reach {}: {e}",
                    url.host_str().unwrap_or("the server")
                )
            })?;
            if resp.status().is_redirection() {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or("a redirect without a location")?;
                url = url
                    .join(loc)
                    .map_err(|e| format!("bad redirect {loc}: {e}"))?;
                continue;
            }
            return Ok(resp);
        }
        Err("too many redirects".into())
    }

    fn http_error(&self, what: &str, st: StatusCode) -> String {
        match st.as_u16() {
            401 | 403 => format!(
                "{what}: the Hub refused access ({st}). A gated or private repository needs HF_TOKEN (or HF_TOKEN_FILE) in the environment."
            ),
            404 => format!("{what}: not found on the Hub (check the repository and file name)"),
            _ => format!("{what}: the Hub answered {st}"),
        }
    }

    /// Read the repository's file list (following `Link: rel=next` pages).
    pub async fn tree(&self, repo: &str) -> Result<Vec<TreeItem>, String> {
        let mut url = format!(
            "{}/api/models/{repo}/tree/main?recursive=1",
            self.policy.base
        );
        let mut all: Vec<TreeItem> = Vec::new();
        for _ in 0..20 {
            let resp = self.get(&url, None).await?;
            if !resp.status().is_success() {
                return Err(self.http_error(&format!("listing {repo}"), resp.status()));
            }
            let next = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let page: Vec<TreeItem> = resp
                .json()
                .await
                .map_err(|e| format!("the Hub's file list for {repo} was not understood: {e}"))?;
            all.extend(page);
            match next {
                Some(n) => url = n,
                None => return Ok(all),
            }
        }
        Ok(all)
    }

    /// Resolve the target to the exact files, sizes and checksums, before anything is fetched.
    pub async fn plan(&self, t: &Target) -> Result<Plan, String> {
        let tree = self.tree(&t.repo).await?;
        let mut files = select(&tree, t.file.as_deref(), t.quant.as_deref())?;
        for f in &files {
            safe_rel(&f.path)?;
        }
        if let (Some(pin), 1) = (&t.pinned_sha256, files.len()) {
            files[0].sha256 = Some(pin.to_ascii_lowercase());
            files[0].pinned = true;
        }
        let total = files.iter().map(|f| f.size).sum();
        Ok(Plan {
            repo: t.repo.clone(),
            files,
            total_bytes: total,
            license: t.license.clone(),
            catalog_id: t.catalog_id.clone(),
        })
    }
}

fn next_link(h: &str) -> Option<String> {
    h.split(',').find_map(|p| {
        let (u, rest) = p.split_once(';')?;
        rest.contains("rel=\"next\"").then(|| {
            u.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

// ---- disk ----------------------------------------------------------------------------------

/// Free bytes on the disk holding `path` (or its nearest existing parent), through `df` or
/// PowerShell with fixed arguments. None when it cannot be told.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let p = p.canonicalize().ok()?;
    #[cfg(unix)]
    {
        let out = super::hw::run_capture(
            Path::new("df"),
            &["-Pk", p.to_str()?],
            Duration::from_secs(5),
        )?;
        parse_df(&out)
    }
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        let drive = s
            .trim_start_matches(r"\\?\")
            .chars()
            .next()
            .filter(char::is_ascii_alphabetic)?;
        let cmd = format!("(Get-PSDrive -Name {}).Free", drive.to_ascii_uppercase());
        let out = super::hw::run_capture(
            Path::new("powershell"),
            &["-NoProfile", "-NonInteractive", "-Command", &cmd],
            Duration::from_secs(10),
        )?;
        super::hw::parse_bytes(&out)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = p;
        None
    }
}

/// `df -Pk`: the fourth column of the last line, in KiB.
pub fn parse_df(out: &str) -> Option<u64> {
    let line = out.lines().rfind(|l| !l.trim().is_empty())?;
    let kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb * 1024)
}

/// Refuse a download that cannot fit (size plus 10%) or exceeds `--max-gb`.
pub fn check_budget(
    plan: &Plan,
    max_bytes: Option<u64>,
    free: Option<u64>,
) -> Result<Option<String>, String> {
    if let Some(m) = max_bytes {
        if plan.total_bytes > m {
            return Err(format!(
                "the download is {} but --max-gb allows {}",
                human_bytes(plan.total_bytes),
                human_bytes(m)
            ));
        }
    }
    match free {
        Some(f) => {
            let need = plan.total_bytes + plan.total_bytes / 10;
            if f < need {
                Err(format!(
                    "not enough free disk space: {} needed (size plus 10%), {} free",
                    human_bytes(need),
                    human_bytes(f)
                ))
            } else {
                Ok(None)
            }
        }
        None => Ok(Some("could not check free disk space; continuing".into())),
    }
}

// ---- the download --------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct Progress {
    pub file: String,
    /// Bytes of the whole plan on disk so far.
    pub done: u64,
    pub total: u64,
    pub resumed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sidecar {
    pub repo: String,
    pub file: String,
    pub sha256: String,
    pub size: u64,
    pub fetched_at: String,
    pub license: String,
    /// `hub` (the Hub's checksum), `pinned` (the catalog's), `computed` (no reference, or an import).
    pub sha256_source: String,
}

pub fn models_dir(data: &Path) -> PathBuf {
    data.join("models")
}

pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut n = file.as_os_str().to_owned();
    n.push(".json");
    PathBuf::from(n)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn stamp() -> String {
    let s = now_secs();
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        iso_date(s),
        (s % 86_400) / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

fn write_sidecar(file: &Path, sc: &Sidecar) -> Result<(), String> {
    let body = serde_json::to_string_pretty(sc).map_err(|e| e.to_string())?;
    std::fs::write(sidecar_path(file), body)
        .map_err(|e| format!("cannot write the checksum record: {e}"))
}

fn content_range_start(h: &str) -> Option<u64> {
    h.strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

impl Pull {
    /// Fetch every file of the plan into `<data>/models/<repo>/`. Returns the model file path
    /// relative to `<data>/models`. Resumes a `.part`; verifies the SHA-256 before renaming.
    pub async fn download(
        &self,
        plan: &Plan,
        data: &Path,
        progress: &mut dyn FnMut(&Progress),
        cancel: Option<&AtomicBool>,
    ) -> Result<String, String> {
        let root = models_dir(data).join(&plan.repo);
        let mut before = 0u64;
        for f in &plan.files {
            self.fetch_one(plan, f, &root, before, progress, cancel)
                .await?;
            before += f.size;
        }
        Ok(plan.model_file())
    }

    async fn fetch_one(
        &self,
        plan: &Plan,
        spec: &FileSpec,
        root: &Path,
        before: u64,
        progress: &mut dyn FnMut(&Progress),
        cancel: Option<&AtomicBool>,
    ) -> Result<(), String> {
        let dest = root.join(&spec.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut part_name = dest.as_os_str().to_owned();
        part_name.push(".part");
        let part = PathBuf::from(part_name);
        let report = |progress: &mut dyn FnMut(&Progress), at: u64, resumed: bool| {
            progress(&Progress {
                file: spec.path.clone(),
                done: before + at,
                total: plan.total_bytes,
                resumed,
            })
        };

        // Already here with the right checksum record: nothing to do.
        if let Ok(m) = std::fs::metadata(&dest) {
            let rec: Option<Sidecar> = std::fs::read_to_string(sidecar_path(&dest))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            if m.len() == spec.size
                && rec.is_some_and(|r| spec.sha256.as_ref().is_none_or(|s| *s == r.sha256))
            {
                report(progress, spec.size, false);
                return Ok(());
            }
        }

        let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if have > spec.size {
            let _ = std::fs::remove_file(&part);
            have = 0;
        }
        let mut hasher = Sha256::new();
        let resumed = have > 0;
        if resumed {
            let mut f = std::fs::File::open(&part).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = f.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }
        report(progress, have, resumed);

        if have < spec.size {
            let url = format!(
                "{}/{}/resolve/main/{}",
                self.policy.base,
                plan.repo,
                url_path(&spec.path)
            );
            let mut resp = self.get(&url, (have > 0).then_some(have)).await?;
            if resp.status() == StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
                let _ = std::fs::remove_file(&part);
                have = 0;
                hasher = Sha256::new();
                resp = self.get(&url, None).await?;
            }
            let st = resp.status();
            if !st.is_success() {
                return Err(self.http_error(&format!("downloading {}", spec.path), st));
            }
            let append = if st == StatusCode::PARTIAL_CONTENT {
                let start = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(content_range_start);
                if start != Some(have) {
                    return Err(
                        "the server resumed from the wrong place; delete the .part file and retry"
                            .into(),
                    );
                }
                true
            } else {
                // The server ignored the range: start over.
                have = 0;
                hasher = Sha256::new();
                false
            };
            let mut out = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&part)
                .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
            let mut last = std::time::Instant::now();
            while let Some(chunk) = resp.chunk().await.map_err(|e| {
                format!("the download was interrupted: {e}. Run the same command again to resume.")
            })? {
                if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                    return Err("cancelled (the partial file is kept, so a retry resumes)".into());
                }
                out.write_all(&chunk)
                    .map_err(|e| format!("cannot write the file (disk full?): {e}"))?;
                hasher.update(&chunk);
                have += chunk.len() as u64;
                if last.elapsed() >= Duration::from_millis(250) {
                    report(progress, have, resumed);
                    last = std::time::Instant::now();
                }
            }
            out.sync_all().map_err(|e| e.to_string())?;
            report(progress, have, resumed);
        }

        if have != spec.size {
            return Err(format!(
                "the download ended early ({} of {}). Run the same command again to resume.",
                human_bytes(have),
                human_bytes(spec.size)
            ));
        }
        let got = hex(&hasher.finalize());
        if let Some(want) = &spec.sha256 {
            if *want != got {
                let _ = std::fs::remove_file(&part);
                return Err(format!(
                    "checksum mismatch for {}: expected {want}, got {got}. The partial file was deleted.",
                    spec.path
                ));
            }
        }
        std::fs::rename(&part, &dest)
            .map_err(|e| format!("cannot move the finished file into place: {e}"))?;
        write_sidecar(
            &dest,
            &Sidecar {
                repo: plan.repo.clone(),
                file: spec.path.clone(),
                sha256: got,
                size: spec.size,
                fetched_at: stamp(),
                license: plan.license.clone(),
                sha256_source: match (&spec.sha256, spec.pinned) {
                    (Some(_), true) => "pinned",
                    (Some(_), false) => "hub",
                    (None, _) => "computed",
                }
                .into(),
            },
        )?;
        Ok(())
    }
}

// ---- import --------------------------------------------------------------------------------

/// `ndcart ai import FILE.gguf`: copy a local model file (for example an Unsloth fine-tune
/// export) into `<data>/models/local/` and record its SHA-256. Returns the path relative to
/// `<data>/models`.
pub fn import(
    data: &Path,
    src: &Path,
    free: &dyn Fn(&Path) -> Option<u64>,
) -> Result<String, String> {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.ends_with(".gguf"))
        .ok_or("the file must end in .gguf")?;
    safe_rel(name)?;
    let mut f =
        std::fs::File::open(src).map_err(|e| format!("cannot open {}: {e}", src.display()))?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)
        .map_err(|_| "the file is too short to be a GGUF model".to_string())?;
    if &magic != b"GGUF" {
        return Err("that is not a GGUF file (it does not start with the GGUF marker)".into());
    }
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let dir = models_dir(data).join("local");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    if let Some(fr) = free(&dir) {
        if fr < size + size / 10 {
            return Err(format!(
                "not enough free disk space: {} needed, {} free",
                human_bytes(size + size / 10),
                human_bytes(fr)
            ));
        }
    }
    let dest = dir.join(name);
    let mut part_name = dest.as_os_str().to_owned();
    part_name.push(".part");
    let part = PathBuf::from(part_name);
    {
        use std::io::Seek;
        f.rewind().map_err(|e| e.to_string())?;
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("cannot write (disk full?): {e}"))?;
            h.update(&buf[..n]);
        }
        out.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        write_sidecar(
            &dest,
            &Sidecar {
                repo: "local".into(),
                file: name.to_string(),
                sha256: hex(&h.finalize()),
                size,
                fetched_at: stamp(),
                license: String::new(),
                sha256_source: "computed".into(),
            },
        )?;
    }
    Ok(format!("local/{name}"))
}

// ---- the command line ----------------------------------------------------------------------

fn ask(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok()
        && matches!(line.trim(), "y" | "Y" | "yes" | "YES")
}

/// `ndcart ai pull <id|repo:File.gguf> [--data-dir D] [--yes] [--max-gb N] [--json] [--allow-non-unsloth]`
pub async fn run_pull(args: Vec<String>) -> Result<(), String> {
    let c = super::common(args)?;
    let (mut target, mut yes, mut max_gb, mut allow) = (None, false, None::<f64>, false);
    let mut it = c.rest.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "--allow-non-unsloth" => allow = true,
            "--max-gb" => {
                max_gb = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--max-gb needs a number")?,
                )
            }
            o if !o.starts_with("--") && target.is_none() => target = Some(o.to_string()),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let target = target.ok_or("usage: ndcart ai pull <catalog-id | unsloth/Repo-GGUF:File.gguf> [--yes] [--max-gb N] [--data-dir D]")?;
    let pull = Pull::from_env();
    let said = |json: bool, ev: serde_json::Value, text: &str| {
        if json {
            println!("{ev}");
        } else {
            eprintln!("{text}");
        }
    };
    let t = parse_target(&target, &catalog::bundled(), allow)?;
    let plan = pull.plan(&t).await?;
    let json = c.json;
    said(
        json,
        serde_json::json!({"event":"plan","repo":plan.repo,"files":plan.files,"total_bytes":plan.total_bytes}),
        &format!(
            "{}: {} file(s), {}",
            plan.repo,
            plan.files.len(),
            human_bytes(plan.total_bytes)
        ),
    );
    std::fs::create_dir_all(models_dir(&c.data))
        .map_err(|e| format!("cannot create the models folder: {e}"))?;
    let max_bytes = max_gb.map(|g| (g * 1e9) as u64);
    if let Some(note) = check_budget(&plan, max_bytes, (pull.free_bytes)(&models_dir(&c.data)))? {
        eprintln!("note: {note}");
    }
    if plan.files.iter().any(|f| f.sha256.is_none()) {
        eprintln!("note: the Hub gave no SHA-256 for a file; the size is checked and the hash is recorded");
    }
    if !yes {
        let q = format!(
            "Download {} from huggingface.co/{}?",
            human_bytes(plan.total_bytes),
            plan.repo
        );
        if !ask(&q) {
            return Err("not downloaded (answer y, or pass --yes in scripts)".into());
        }
    }
    let mut last = std::time::Instant::now() - Duration::from_secs(5);
    let mut progress = |p: &Progress| {
        if last.elapsed() < Duration::from_secs(1) && p.done < p.total {
            return;
        }
        last = std::time::Instant::now();
        if json {
            println!(
                "{}",
                serde_json::json!({"event":"progress","file":p.file,"done":p.done,"total":p.total})
            );
        } else {
            eprint!("\r{} / {}   ", human_bytes(p.done), human_bytes(p.total));
        }
    };
    let rel = pull.download(&plan, &c.data, &mut progress, None).await?;
    if !json {
        eprintln!();
    }
    said(
        json,
        serde_json::json!({"event":"done","model_file":rel}),
        &format!("done: {}", models_dir(&c.data).join(&rel).display()),
    );
    Ok(())
}

/// `ndcart ai import FILE.gguf [--data-dir D]`
pub fn run_import(args: Vec<String>) -> Result<(), String> {
    let c = super::common(args)?;
    let [file] = c.rest.as_slice() else {
        return Err("usage: ndcart ai import FILE.gguf [--data-dir D]".into());
    };
    let rel = import(&c.data, Path::new(file), &free_bytes)?;
    println!("imported: {}", models_dir(&c.data).join(rel).display());
    Ok(())
}

#[cfg(test)]
mod tests;
