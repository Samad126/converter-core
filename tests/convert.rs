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

#[tokio::test]
async fn native_image_roundtrip_needs_no_external_tools() {
    let d = tmp("img");
    let src = d.join("pic.png");
    image::RgbaImage::from_fn(40, 30, |x, y| image::Rgba([(x * 6) as u8, (y * 8) as u8, 128, 255])).save(&src).unwrap();
    for (target, ext) in [("bmp", "bmp"), ("gif", "gif"), ("tiff", "tiff"), ("webp", "webp"), ("ico", "ico"),
                          ("jpg-image", "jpg"), ("qoi", "qoi"), ("tga", "tga")] {
        let out = convert_file(&cfg(), &src, target, ConvertOptions::default()).await
            .unwrap_or_else(|e| panic!("png -> {target}: {e}"));
        assert_eq!(out[0].extension().unwrap(), ext);
        let back = image::open(&out[0]).unwrap_or_else(|e| panic!("{target} unreadable: {e}"));
        assert_eq!((back.width(), back.height()), (40, 30), "{target} dimensions");
        // and back to png from the converted file
        let again = convert_file(&cfg(), &out[0], "png-image", ConvertOptions { out_dir: Some(d.join("back")), ocr: None }).await
            .unwrap_or_else(|e| panic!("{target} -> png: {e}"));
        assert!(again[0].exists());
    }
}

#[tokio::test]
async fn ico_over_256_is_refused_not_downscaled() {
    let d = tmp("ico");
    let src = d.join("big.png");
    image::RgbaImage::new(300, 300).save(&src).unwrap();
    let e = convert_file(&cfg(), &src, "ico", ConvertOptions::default()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::EConvertFailed);
    assert!(e.message.contains("256"));
}

/// Needs python + pyarrow (the Arrow formats have no pure-Rust route); skipped when they are not installed.
fn have_pyarrow() -> bool {
    engines::resolve(engines::tool("python").unwrap(), &cfg(), None)
        .is_some_and(|p| std::process::Command::new(p).args(["-c", "import pyarrow"]).output().is_ok_and(|o| o.status.success()))
}

#[tokio::test]
async fn csv_survives_parquet_orc_and_feather_round_trips() {
    if !have_pyarrow() { eprintln!("SKIP: python with pyarrow not available"); return; }
    let d = tmp("arrow");
    std::fs::write(d.join("t.csv"), "name,city\nAnn,Baku\nƏli,Gəncə\n").unwrap();
    for fmt in ["parquet", "orc", "feather"] {
        let packed = convert_file(&cfg(), &d.join("t.csv"), fmt, ConvertOptions { out_dir: Some(d.join(fmt)), ocr: None }).await.unwrap_or_else(|e| panic!("csv -> {fmt}: {e}"));
        let back = convert_file(&cfg(), &packed[0], "csv", ConvertOptions { out_dir: Some(d.join(format!("{fmt}-back"))), ocr: None }).await.unwrap_or_else(|e| panic!("{fmt} -> csv: {e}"));
        assert_eq!(std::fs::read_to_string(&back[0]).unwrap(), "name,city\r\nAnn,Baku\r\nƏli,Gəncə\r\n", "{fmt}");
    }
}

#[tokio::test]
async fn non_tabular_json_cannot_become_parquet() {
    if !have_pyarrow() { eprintln!("SKIP: python with pyarrow not available"); return; }
    let d = tmp("arrow-bad");
    std::fs::write(d.join("x.json"), r#"{"not":"a table"}"#).unwrap();
    let e = convert_file(&cfg(), &d.join("x.json"), "parquet", ConvertOptions::default()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ENotTabular);
}

/// New LibreOffice-routed sources (templates, flat OpenDocument): the filter names are only trusted once a real file
/// went through the real engine.
#[tokio::test]
async fn libreoffice_routes_for_the_extra_office_formats() {
    if !have("libreoffice") { eprintln!("SKIP: libreoffice not installed"); return; }
    let fx = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let d = tmp("extras");
    for (src, target, magic) in [("sample.fods", "csv", &b"name,qty"[..]), ("deck.otp", "pdf", b"%PDF"), ("sample.ott", "docx", b"PK"), ("sample.fodt", "pdf", b"%PDF"), ("sample.ots", "xlsx", b"PK"), ("sample.xltx", "pdf", b"%PDF"), ("any_sheets.xlsb", "csv", b""), ("wp6.wpd", "pdf", b"%PDF"), ("works.wps", "txt", b""), ("pages4.pages", "pdf", b"%PDF"), ("visio.vsdx", "pdf", b"%PDF"), ("visio.vsd", "pdf", b"%PDF")] {
        std::fs::copy(fx.join(src), d.join(src)).unwrap();
        let out = convert_file(&cfg(), &d.join(src), target, ConvertOptions::default()).await.unwrap_or_else(|e| panic!("{src} -> {target}: {e:?}"));
        let bytes = std::fs::read(&out[0]).unwrap();
        assert!(bytes.starts_with(magic) && !bytes.is_empty(), "{src} -> {target}");
    }
}
