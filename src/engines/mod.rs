//! External-tool discovery. Unlike the server's fail-fast preflight, a missing
//! tool is a normal state here: it disables the conversions that need it and
//! the UI offers to install it.
pub mod manager;

use crate::config::Config;
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub struct Tool {
    /// Stable id, also the directory name under `engines_dir`.
    pub name: &'static str,
    pub used_for: &'static str,
    /// Executable names to look for, in order.
    pub bins: &'static [&'static str],
    pub version_args: &'static [&'static str],
    pub hint_linux: &'static str,
    pub hint_macos: &'static str,
    pub hint_windows: &'static str,
}

pub const TOOLS: &[Tool] = &[
    Tool { name: "libreoffice", used_for: "optional: highest-fidelity Office and legacy files", bins: &["soffice", "soffice.exe", "libreoffice"], version_args: &["--version"],
        hint_linux: "apt-get install -y libreoffice-writer libreoffice-calc libreoffice-impress libreoffice-draw",
        hint_macos: "brew install --cask libreoffice", hint_windows: "winget install --id TheDocumentFoundation.LibreOffice" },
    Tool { name: "poppler", used_for: "optional: faster PDF page images", bins: &["pdftoppm", "pdftoppm.exe"], version_args: &["-v"],
        hint_linux: "apt-get install -y poppler-utils", hint_macos: "brew install poppler", hint_windows: "winget install --id oschwartz10612.Poppler" },
    Tool { name: "pandoc", used_for: "markup formats (md/rst/tex/org/...)", bins: &["pandoc", "pandoc.exe"], version_args: &["--version"],
        hint_linux: "apt-get install -y pandoc", hint_macos: "brew install pandoc", hint_windows: "winget install --id JohnMacFarlane.Pandoc" },
    Tool { name: "qpdf", used_for: "PDF protect/unlock/repair/compress", bins: &["qpdf", "qpdf.exe"], version_args: &["--version"],
        hint_linux: "apt-get install -y qpdf", hint_macos: "brew install qpdf", hint_windows: "winget install --id QPDF.QPDF" },
    Tool { name: "7zip", used_for: "7z, bz2, zst and other archives", bins: &["7zz", "7z", "7z.exe"], version_args: &[],
        hint_linux: "apt-get install -y p7zip-full", hint_macos: "brew install sevenzip", hint_windows: "winget install --id 7zip.7zip" },
    Tool { name: "ffmpeg", used_for: "image fallback, audio and video", bins: &["ffmpeg", "ffmpeg.exe"], version_args: &["-version"],
        hint_linux: "apt-get install -y ffmpeg", hint_macos: "brew install ffmpeg", hint_windows: "winget install --id Gyan.FFmpeg" },
    Tool { name: "yt-dlp", used_for: "YouTube downloads", bins: &["yt-dlp", "yt-dlp.exe"], version_args: &["--version"],
        hint_linux: "pip install \"yt-dlp[default]\"", hint_macos: "brew install yt-dlp", hint_windows: "winget install --id yt-dlp.yt-dlp" },
    Tool { name: "heif", used_for: "HEIC/HEIF photos", bins: &["heif-convert", "heif-convert.exe"], version_args: &[],
        hint_linux: "apt-get install -y libheif-examples", hint_macos: "brew install libheif", hint_windows: "no standard Windows build" },
    Tool { name: "zstd", used_for: ".zst / .tar.zst archives", bins: &["zstd", "zstd.exe"], version_args: &["--version"],
        hint_linux: "apt-get install -y zstd", hint_macos: "brew install zstd", hint_windows: "winget install --id Meta.Zstandard" },
    Tool { name: "assimp", used_for: "3D models (obj/stl/ply/glb/3mf)", bins: &["assimp", "assimp.exe"], version_args: &["version"],
        hint_linux: "apt-get install -y assimp-utils", hint_macos: "brew install assimp", hint_windows: "no standard winget package" },
    Tool { name: "calibre", used_for: "ebooks (epub/mobi/azw3/fb2/...)", bins: &["ebook-convert", "ebook-convert.exe"], version_args: &["--version"],
        hint_linux: "apt-get install -y calibre", hint_macos: "brew install --cask calibre", hint_windows: "winget install --id calibre.calibre" },
    Tool { name: "tesseract", used_for: "OCR for scanned PDFs", bins: &["tesseract", "tesseract.exe"], version_args: &["--version"],
        hint_linux: "apt-get install -y tesseract-ocr", hint_macos: "brew install tesseract", hint_windows: "winget install --id UB-Mannheim.TesseractOCR" },
    Tool { name: "python", used_for: "optional: layout-faithful PDF to Word/Excel, Parquet/ORC, WOFF2, compare, redact", bins: &["python3", "python", "python.exe"], version_args: &["--version"],
        hint_linux: "apt-get install -y python3 python3-pip", hint_macos: "brew install python3", hint_windows: "winget install --id Python.Python.3.12" },
];

pub fn tool(name: &str) -> Option<&'static Tool> { TOOLS.iter().find(|t| t.name == name) }

impl Tool {
    pub fn hint(&self) -> &'static str {
        if cfg!(target_os = "windows") { self.hint_windows }
        else if cfg!(target_os = "macos") { self.hint_macos }
        else { self.hint_linux }
    }
}

/// Well-known install dirs not always on a GUI app's PATH (esp. macOS/Windows).
fn extra_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![];
    if cfg!(target_os = "macos") {
        for d in ["/opt/homebrew/bin", "/usr/local/bin", "/Applications/LibreOffice.app/Contents/MacOS"] { v.push(d.into()); }
    }
    if cfg!(target_os = "windows") {
        for d in [r"C:\Program Files\LibreOffice\program", r"C:\Program Files\7-Zip", r"C:\Program Files\calibre2",
                  r"C:\Program Files\Tesseract-OCR", r"C:\Program Files\Pandoc", r"C:\Program Files\Calibre2"] { v.push(d.into()); }
        // winget installs that ship no PATH entry: portable-package shims, per-user Pandoc, and versioned qpdf folders.
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            v.push(local.join(r"Microsoft\WinGet\Links"));
            v.push(local.join("Pandoc"));
        }
        if let Ok(rd) = std::fs::read_dir(r"C:\Program Files") {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().to_lowercase().starts_with("qpdf") { v.push(e.path().join("bin")); }
            }
        }
    }
    v
}

/// Finds `bin` somewhere under `dir` (engines unpack into versioned subfolders).
fn find_in_dir(dir: &Path, bin: &str, depth: u32) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { subdirs.push(p); }
        else if p.file_name().is_some_and(|n| n == bin) { return Some(p); }
    }
    if depth == 0 { return None; }
    subdirs.into_iter().find_map(|d| find_in_dir(&d, bin, depth - 1))
}

/// Lookup order: user-picked path → our own downloaded copy → PATH + well-known dirs.
pub fn resolve(tool: &Tool, cfg: &Config, user_path: Option<&Path>) -> Option<PathBuf> {
    // `CONVERTER_BIN_<NAME>` (e.g. CONVERTER_BIN_PYTHON, CONVERTER_BIN_7ZIP) wins over everything.
    let var = format!("CONVERTER_BIN_{}", tool.name.to_uppercase().replace('-', "_"));
    if let Some(p) = std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_file()) { return Some(p); }
    if let Some(p) = user_path { if p.is_file() { return Some(p.to_path_buf()); } }
    let own = cfg.engines_dir.join(tool.name);
    for bin in tool.bins {
        if let Some(p) = find_in_dir(&own, bin, 4) { return Some(p); }
    }
    for bin in tool.bins {
        if let Ok(p) = which::which(bin) { return Some(p); }
        for d in extra_dirs() { let p = d.join(bin); if p.is_file() { return Some(p); } }
    }
    // winget unpacks some tools (Poppler, FFmpeg, zstd...) under Packages\<id>_<source>\<version>\... with no PATH entry or shim.
    if cfg!(target_os = "windows") {
        if let Some(pk) = std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join(r"Microsoft\WinGet\Packages")) {
            for bin in tool.bins {
                if let Some(p) = find_in_dir(&pk, bin, 4) { return Some(p); }
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Status {
    pub name: &'static str,
    pub used_for: &'static str,
    pub path: Option<PathBuf>,
    /// First line the tool printed for its version args (may be empty).
    pub version: Option<String>,
    pub hint: &'static str,
}

impl Status {
    pub fn available(&self) -> bool { self.path.is_some() }
}

pub async fn probe(tool: &'static Tool, cfg: &Config) -> Status {
    let path = resolve(tool, cfg, None);
    let mut version = None;
    if let Some(p) = &path {
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(p).args(tool.version_args)
                .stdin(Stdio::null()).stderr(Stdio::piped()).stdout(Stdio::piped()).output(),
        ).await;
        if let Ok(Ok(o)) = out {
            let text = if o.stdout.is_empty() { o.stderr } else { o.stdout };
            version = String::from_utf8_lossy(&text).lines().next().map(|l| l.trim().to_string());
        }
    }
    Status { name: tool.name, used_for: tool.used_for, path, version, hint: tool.hint() }
}

pub async fn probe_all(cfg: &Config) -> Vec<Status> {
    let mut out = vec![];
    for t in TOOLS { out.push(probe(t, cfg).await); }
    out
}

/// Tool each conversion route needs (None = pure Rust, nothing to install).
pub fn required_tool(engine: crate::formats::Engine) -> Option<&'static str> {
    use crate::formats::Engine::*;
    match engine {
        Soffice => Some("libreoffice"), Pandoc => Some("pandoc"), Archive => Some("7zip"),
        Ffmpeg => Some("ffmpeg"), Heif => Some("heif"), Assimp => Some("assimp"), Ebook => Some("calibre"),
        PdfEngine | Font => Some("python"), Extract | Data | Email | Native => None,
    }
}
