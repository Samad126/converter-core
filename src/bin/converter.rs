use clap::{Parser, Subcommand};
use converter_core::{config::Config, engines, formats};

#[derive(Parser)]
#[command(name = "converter", about = "File converter (Rust port)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// What an extension (e.g. .docx) can become; without args, the whole matrix size.
    Formats { extension: Option<String> },
    /// Convert a file: converter convert <target> <file> [--out DIR] (same order as the Node CLI)
    Convert { target: String, input: std::path::PathBuf, #[arg(long)] out: Option<std::path::PathBuf> },
    /// Convert audio/video with progress: converter media <target> <file> [--out DIR]
    Media { target: String, input: std::path::PathBuf, #[arg(long)] out: Option<std::path::PathBuf> },
    /// Download a YouTube video: converter youtube <mp3|mp4> <url> [--out DIR]
    Youtube { target: String, url: String, #[arg(long)] out: Option<std::path::PathBuf> },
    /// Show which external tools are installed and how to get the missing ones.
    Doctor,
}

#[tokio::main]
async fn main() {
    match Cli::parse().cmd {
        Cmd::Formats { extension: Some(e) } => {
            let e = if e.starts_with('.') { e } else { format!(".{e}") };
            let t = formats::targets_for(&e.to_lowercase());
            if t.is_empty() { eprintln!("{e}: not a supported source"); std::process::exit(1); }
            println!("{e} -> {}", t.join(", "));
        }
        Cmd::Formats { extension: None } => {
            let m = formats::matrix();
            println!("{} sources, {} targets, {} media formats", m.sources.len(), m.targets.len(), m.media.len());
        }
        Cmd::Convert { input, target, out } => {
            let cfg = Config::default();
            let opts = converter_core::ConvertOptions { out_dir: out, ocr: None };
            match converter_core::convert_file(&cfg, &input, &target, opts).await {
                Ok(files) => for f in files { println!("{}", f.display()); },
                Err(e) => { eprintln!("{}: {}", e.code.as_str(), e.message); std::process::exit(1); }
            }
        }
        Cmd::Media { target, input, out } => {
            let cfg = Config::default();
            let progress: converter_core::progress::ProgressFn = std::sync::Arc::new(|p| {
                if let converter_core::progress::Progress::Fraction(f) = p { eprint!("\r{:>3.0}%", f * 100.0); }
            });
            let opts = converter_core::ConvertOptions { out_dir: out, ocr: None };
            match converter_core::media::convert_media(&cfg, &input, &target, opts, progress).await {
                Ok(f) => { eprintln!(); println!("{}", f.display()); }
                Err(e) => { eprintln!("\n{}: {}", e.code.as_str(), e.message); std::process::exit(1); }
            }
        }
        Cmd::Youtube { target, url, out } => {
            let cfg = Config::default();
            let Some(t) = converter_core::youtube::Target::parse(&target) else { eprintln!("target must be mp3 or mp4"); std::process::exit(2) };
            let dir = out.unwrap_or_else(|| std::env::current_dir().unwrap());
            let progress: converter_core::progress::ProgressFn = std::sync::Arc::new(|p| {
                if let converter_core::progress::Progress::Fraction(f) = p { eprint!("\r{:>3.0}%", f * 100.0); }
            });
            match converter_core::youtube::download(&cfg, &url, t, &dir, progress).await {
                Ok(f) => { eprintln!(); println!("{}", f.display()); }
                Err(e) => { eprintln!("\n{}: {}", e.code.as_str(), e.message); std::process::exit(1); }
            }
        }
        Cmd::Doctor => {
            let cfg = Config::default();
            let mut missing = 0;
            for s in engines::probe_all(&cfg).await {
                if s.available() {
                    println!("ok       {:<12} {}", s.name, s.version.as_deref().unwrap_or(""));
                } else {
                    missing += 1;
                    println!("MISSING  {:<12} {} -> {}", s.name, s.used_for, s.hint);
                }
            }
            if missing > 0 { std::process::exit(1); }
        }
    }
}
