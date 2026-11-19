use converter_core::{convert_file, engines, Config, ConvertOptions, ErrorCode};

fn cfg() -> Config { Config::default() }
fn have(tool: &str) -> bool { engines::resolve(engines::tool(tool).unwrap(), &cfg(), None).is_some() }

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("conv-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn txt_to_pdf_and_name_clash() {
    if !have("libreoffice") { eprintln!("SKIP: libreoffice not installed"); return; }
    let d = tmp("pdf");
    std::fs::write(d.join("a.txt"), "hello\n").unwrap();
    let a = convert_file(&cfg(), &d.join("a.txt"), "pdf", ConvertOptions::default()).await.unwrap();
    let b = convert_file(&cfg(), &d.join("a.txt"), "pdf", ConvertOptions::default()).await.unwrap();
    assert_eq!(a[0].file_name().unwrap(), "a.pdf");
    assert_eq!(b[0].file_name().unwrap(), "a (1).pdf");
    assert!(std::fs::read(&a[0]).unwrap().starts_with(b"%PDF"));
}

#[tokio::test]
async fn unsupported_pair_names_alternatives() {
    let d = tmp("bad");
    std::fs::write(d.join("a.txt"), "x").unwrap();
    let e = convert_file(&cfg(), &d.join("a.txt"), "png", ConvertOptions::default()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::EUnsupportedTarget);
    assert!(e.message.contains("pdf"));
}

#[tokio::test]
async fn missing_engine_is_reported_not_crashed() {
    if have("pandoc") { eprintln!("SKIP: pandoc installed"); return; }
    let d = tmp("md");
    std::fs::write(d.join("n.md"), "# t").unwrap();
    let e = convert_file(&cfg(), &d.join("n.md"), "docx", ConvertOptions::default()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::EEngineMissing);
    assert_eq!(e.engine.as_deref(), Some("pandoc"));
}
