//! yt-dlp integration: locate or install the tools, probe a URL, download its audio track.
//!
//! No ffmpeg. We ask yt-dlp for YouTube's native audio stream (m4a, AAC) and decode it ourselves,
//! so there is no re-encode. Trimming happens in Hotmic on the decoded audio.
//!
//! Two tools are managed, both single executables fetched from their official GitHub releases
//! and verified against the published SHA-256:
//! - yt-dlp itself.
//! - Deno, the JavaScript runtime yt-dlp needs to solve YouTube's player challenges. Without it
//!   YouTube answers with "sign in to confirm you're not a bot" or hides formats.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const YTDLP_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const DENO_BASE: &str = "https://github.com/denoland/deno/releases/latest/download";
#[cfg(windows)]
const YTDLP_EXE: &str = "yt-dlp.exe";
#[cfg(not(windows))]
const YTDLP_EXE: &str = "yt-dlp";
#[cfg(windows)]
const DENO_EXE: &str = "deno.exe";
#[cfg(not(windows))]
const DENO_EXE: &str = "deno";
#[cfg(windows)]
const DENO_ZIP: &str = "deno-x86_64-pc-windows-msvc.zip";
#[cfg(all(not(windows), target_os = "linux"))]
const DENO_ZIP: &str = "deno-x86_64-unknown-linux-gnu.zip";
#[cfg(all(not(windows), target_os = "macos"))]
const DENO_ZIP: &str = "deno-aarch64-apple-darwin.zip";

/// Where Hotmic keeps tools it downloaded itself.
pub fn bin_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    base.join("hotmic").join("bin")
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStatus {
    pub ytdlp_path: Option<String>,
    pub ytdlp_version: Option<String>,
    pub deno_path: Option<String>,
    pub deno_version: Option<String>,
    pub bin_dir: String,
    /// Everything needed for YouTube is present.
    pub ready: bool,
    pub download_size_mb: u32,
}

/// Per-call options.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Options {
    /// Browser whose cookies yt-dlp should use ("chrome", "edge", "firefox", ...). Lets
    /// logged-in users through YouTube's bot checks and age gates.
    pub cookies_from_browser: Option<String>,
}

fn command(path: &Path) -> Command {
    let mut c = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

fn find_tool(exe: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            candidates.push(dir.join(exe));
        }
    }
    candidates.push(bin_dir().join(exe));
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    let on_path = PathBuf::from(exe);
    if version(&on_path).is_some() {
        return Some(on_path);
    }
    None
}

/// Look next to our own exe, in the Hotmic bin dir, then on PATH.
pub fn find() -> Option<PathBuf> {
    find_tool(YTDLP_EXE)
}

pub fn find_deno() -> Option<PathBuf> {
    find_tool(DENO_EXE)
}

pub fn version(path: &Path) -> Option<String> {
    let out = command(path).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // deno prints "deno 2.x.y (...)\nv8 ...", yt-dlp prints just the version
    Some(text.lines().next()?.trim().to_string())
}

pub fn status() -> ToolStatus {
    let yt = find();
    let deno = find_deno();
    let ready = yt.is_some() && deno.is_some();
    ToolStatus {
        ytdlp_version: yt.as_deref().and_then(version),
        ytdlp_path: yt.map(|p| p.to_string_lossy().into_owned()),
        deno_version: deno.as_deref().and_then(version),
        deno_path: deno.map(|p| p.to_string_lossy().into_owned()),
        bin_dir: bin_dir().to_string_lossy().into_owned(),
        ready,
        download_size_mb: 17 + 45,
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent("hotmic")
        .timeout(std::time::Duration::from_secs(600))
        .build()?)
}

/// Fetch `url` and check its SHA-256 against `expected_hex`.
fn fetch_verified(client: &reqwest::blocking::Client, url: &str, expected_hex: &str) -> Result<Vec<u8>> {
    let bytes = client
        .get(url)
        .send()
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()?
        .bytes()?
        .to_vec();
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected_hex.to_lowercase() {
        bail!("checksum mismatch for {url}: expected {expected_hex}, got {actual}");
    }
    Ok(bytes)
}

/// Find the SHA-256 for `file` in a checksum listing. Accepts the classic `hash  name` lines
/// (sha256sum, with or without `*`), PowerShell's `Algorithm Hash Path` table, and a bare hash.
/// A line that names the file wins; otherwise the first 64-hex-digit token is used.
fn find_hash(listing: &str, file: &str) -> Option<String> {
    let is_hex64 = |t: &str| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit());
    let hex_in = |l: &str| {
        l.split_whitespace()
            .map(|t| t.trim_start_matches('*'))
            .find(|t| is_hex64(t))
            .map(|t| t.to_lowercase())
    };
    let lower = file.to_lowercase();
    listing
        .lines()
        .filter(|l| l.to_lowercase().contains(&lower))
        .find_map(hex_in)
        .or_else(|| listing.lines().find_map(hex_in))
}

fn place(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let target = dir.join(name);
    let tmp = dir.join(format!("{name}.part"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {name}"))?;
    if target.exists() {
        let _ = std::fs::remove_file(&target);
    }
    std::fs::rename(&tmp, &target).with_context(|| format!("moving {name} into place"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    Ok(target)
}

/// Download the latest yt-dlp release into [`bin_dir`], verifying it against the published SHA-256.
pub fn install_ytdlp() -> Result<PathBuf> {
    let dir = bin_dir();
    std::fs::create_dir_all(&dir).context("creating bin dir")?;
    let client = client()?;
    let sums = client
        .get(format!("{YTDLP_BASE}/SHA2-256SUMS"))
        .send()
        .context("fetching yt-dlp checksum list")?
        .error_for_status()?
        .text()?;
    let expected =
        find_hash(&sums, YTDLP_EXE).ok_or_else(|| anyhow!("{YTDLP_EXE} not in SHA2-256SUMS"))?;
    let bytes = fetch_verified(&client, &format!("{YTDLP_BASE}/{YTDLP_EXE}"), &expected)?;
    place(&dir, YTDLP_EXE, &bytes)
}

/// Download the latest Deno release zip, verify it, and unpack the single executable.
pub fn install_deno() -> Result<PathBuf> {
    let dir = bin_dir();
    std::fs::create_dir_all(&dir).context("creating bin dir")?;
    let client = client()?;
    let sum = client
        .get(format!("{DENO_BASE}/{DENO_ZIP}.sha256sum"))
        .send()
        .context("fetching deno checksum")?
        .error_for_status()?
        .text()?;
    let expected = find_hash(&sum, DENO_ZIP)
        .or_else(|| sum.split_whitespace().next().map(str::to_string))
        .ok_or_else(|| anyhow!("deno checksum file is empty"))?;
    let bytes = fetch_verified(&client, &format!("{DENO_BASE}/{DENO_ZIP}"), &expected)?;

    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).context("reading deno zip")?;
    let mut exe: Option<Vec<u8>> = None;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.name().rsplit('/').next() == Some(DENO_EXE) {
            let mut buf = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut buf)?;
            exe = Some(buf);
            break;
        }
    }
    let exe = exe.ok_or_else(|| anyhow!("{DENO_EXE} not found inside {DENO_ZIP}"))?;
    place(&dir, DENO_EXE, &exe)
}

/// Install whatever is missing. Returns the new status.
pub fn install() -> Result<ToolStatus> {
    if find().is_none() {
        install_ytdlp()?;
    }
    if find_deno().is_none() {
        install_deno()?;
    }
    Ok(status())
}

/// `yt-dlp -U`. Returns its output.
pub fn update(path: &Path) -> Result<String> {
    let out = command(path).arg("-U").output().context("running yt-dlp -U")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        bail!("{}", text.trim());
    }
    Ok(text.trim().to_string())
}

/// Arguments every yt-dlp call gets: JS runtime, cookies, quiet warnings.
fn base_args(cmd: &mut Command, opts: &Options) {
    cmd.args(["--no-playlist", "--no-warnings"]);
    if let Some(deno) = find_deno() {
        cmd.arg("--js-runtimes");
        cmd.arg(format!("deno:{}", deno.display()));
    }
    if let Some(b) = opts.cookies_from_browser.as_deref().filter(|b| !b.is_empty()) {
        cmd.args(["--cookies-from-browser", b]);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub duration_secs: Option<f32>,
    pub thumbnail: Option<String>,
    pub uploader: Option<String>,
    pub webpage_url: Option<String>,
}

/// Metadata only, no download. Takes a few seconds (yt-dlp fetches the watch page).
pub fn probe(path: &Path, url: &str, opts: &Options) -> Result<VideoInfo> {
    let mut cmd = command(path);
    base_args(&mut cmd, opts);
    let out = cmd
        .args(["-J", "--", url])
        .output()
        .context("running yt-dlp -J")?;
    if !out.status.success() {
        bail!("{}", tidy_error(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("yt-dlp JSON")?;
    Ok(VideoInfo {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        title: v["title"].as_str().unwrap_or("Untitled").to_string(),
        duration_secs: v["duration"].as_f64().map(|d| d as f32),
        thumbnail: v["thumbnail"].as_str().map(str::to_string),
        uploader: v["uploader"].as_str().map(str::to_string),
        webpage_url: v["webpage_url"].as_str().map(str::to_string),
    })
}

/// Download the best native audio track into `out_dir`. Calls `progress(percent, line)` as
/// yt-dlp reports it. Returns the path of the finished file.
pub fn download(
    path: &Path,
    url: &str,
    out_dir: &Path,
    opts: &Options,
    mut progress: impl FnMut(f32, &str),
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).context("creating download dir")?;
    let template = out_dir.join("%(title).80s [%(id)s].%(ext)s");
    let mut cmd = command(path);
    base_args(&mut cmd, opts);
    let mut child = cmd
        .args([
            "-f",
            "bestaudio[ext=m4a]/bestaudio/best",
            "--no-part",
            "--newline",
            "--progress",
            "--windows-filenames",
            "--no-simulate",
            "--print",
            "after_move:filepath",
            "-o",
        ])
        .arg(&template)
        .arg("--")
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting yt-dlp")?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            s.push_str(&line);
            s.push('\n');
        }
        s
    });

    let mut final_path: Option<PathBuf> = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[download]") {
            if let Some(p) = parse_percent(&line) {
                progress(p, &line);
            }
            continue;
        }
        if line.starts_with('[') {
            continue;
        }
        let candidate = PathBuf::from(&line);
        if candidate.is_file() {
            final_path = Some(candidate);
        }
    }
    let status = child.wait().context("waiting for yt-dlp")?;
    let stderr_text = err_thread.join().unwrap_or_default();
    if !status.success() {
        bail!("{}", tidy_error(&stderr_text));
    }
    final_path.ok_or_else(|| anyhow!("yt-dlp finished but did not report an output file"))
}

fn parse_percent(line: &str) -> Option<f32> {
    // "[download]  45.3% of   3.21MiB at  1.02MiB/s ETA 00:02"
    let rest = line.strip_prefix("[download]")?.trim_start();
    let pct = rest.split('%').next()?.trim();
    pct.parse::<f32>().ok()
}

fn tidy_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let pick = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR"))
        .or_else(|| lines.last())
        .copied()
        .unwrap_or("yt-dlp failed");
    pick.trim_start_matches("ERROR:").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_parsing() {
        assert_eq!(parse_percent("[download]  45.3% of 3.21MiB at 1.02MiB/s"), Some(45.3));
        assert_eq!(parse_percent("[download] 100% of 3.21MiB in 00:03"), Some(100.0));
        assert_eq!(parse_percent("[download] Destination: x.m4a"), None);
    }

    #[test]
    fn error_tidying() {
        assert_eq!(
            tidy_error("WARNING: x\nERROR: [youtube] abc: Video unavailable\n"),
            "[youtube] abc: Video unavailable"
        );
    }

    #[test]
    fn hash_listing() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let l = format!("{a}  yt-dlp.exe\n{b} *deno-x86_64-pc-windows-msvc.zip\n");
        assert_eq!(find_hash(&l, "yt-dlp.exe").as_deref(), Some(a.as_str()));
        assert_eq!(find_hash(&l, "deno-x86_64-pc-windows-msvc.zip").as_deref(), Some(b.as_str()));
        // unknown file name falls back to the first hash
        assert_eq!(find_hash(&l, "nope").as_deref(), Some(a.as_str()));
        assert_eq!(find_hash("no hashes here", "x"), None);

        // PowerShell Get-FileHash layout, as published by Deno
        let ps = "\r\nAlgorithm : SHA256\r\nHash      : A0C3101B4158D1DFB7D6A78A7BF0F3DE80C96BB423C152BEEC8BEB22786F2238\r\nPath      : C:\\a\\deno\\deno-x86_64-pc-windows-msvc.zip\r\n";
        assert_eq!(
            find_hash(ps, "deno-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("a0c3101b4158d1dfb7d6a78a7bf0f3de80c96bb423c152beec8beb22786f2238")
        );
    }
}
