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

/// `.lzma` and `.zipx` are 7-Zip routes; checked against the real 7-Zip with files it wrote itself.
#[tokio::test]
async fn seven_zip_routes_for_lzma_and_zipx() {
    if !have("7zip") { eprintln!("SKIP: 7zip not installed"); return; }
    let d = tmp("7zx");
    std::fs::write(d.join("payload.txt"), "hello 7z\n").unwrap();
    let bin = engines::resolve(engines::tool("7zip").unwrap(), &cfg(), None).unwrap();
    // .lzma written with liblzma (7-Zip cannot create it), .zipx written by 7-Zip (a zip using LZMA).
    {
        let opts = xz2::stream::LzmaOptions::new_preset(3).unwrap();
        let stream = xz2::stream::Stream::new_lzma_encoder(&opts).unwrap();
        let mut e = xz2::write::XzEncoder::new_stream(std::fs::File::create(d.join("a.lzma")).unwrap(), stream);
        std::io::Write::write_all(&mut e, b"hello lzma\n").unwrap();
        e.finish().unwrap();
    }
    let st = std::process::Command::new(&bin).args(["a", "-tzip", "-mm=LZMA"]).arg(d.join("b.zipx")).arg(d.join("payload.txt")).current_dir(&d).stdout(std::process::Stdio::null()).status().unwrap();
    assert!(st.success(), "7z could not make b.zipx");
    for name in ["a.lzma", "b.zipx"] {
        let out = convert_file(&cfg(), &d.join(name), "zip", ConvertOptions::default()).await.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(std::fs::metadata(&out[0]).unwrap().len() > 20, "{name}");
    }
}

/// Container formats added on top of the backend's list: each was checked with the zero-flag `ffmpeg -y -i in out`
/// the media pipeline runs, both as target and as source. (.3gp/.amr/.mxf/.dv fail that default command and stay out.)
#[tokio::test]
async fn extra_media_containers_round_trip_through_ffmpeg() {
    if !have("ffmpeg") { eprintln!("SKIP: ffmpeg not installed"); return; }
    use converter_core::media::convert_media;
    let ff = engines::resolve(engines::tool("ffmpeg").unwrap(), &cfg(), None).unwrap();
    let d = tmp("media");
    let run = |args: &[&str]| assert!(std::process::Command::new(&ff).args(["-y", "-loglevel", "error"]).args(args).stdout(std::process::Stdio::null()).status().unwrap().success());
    run(&["-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-ar", "44100", d.join("src.wav").to_str().unwrap()]);
    run(&["-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=15", "-f", "lavfi", "-i", "sine=duration=1", "-shortest", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", d.join("src.mp4").to_str().unwrap()]);
    let progress: converter_core::progress::ProgressFn = std::sync::Arc::new(|_| {});
    for (src, targets, back) in [("src.wav", ["mka", "mp2", "wv", "tta"], "wav"), ("src.mp4", ["mts", "m2ts", "vob", "mkv"], "mp4")] {
        for t in targets {
            let out = convert_media(&cfg(), &d.join(src), t, ConvertOptions { out_dir: Some(d.join("o")), ..Default::default() }, progress.clone()).await.unwrap_or_else(|e| panic!("{src} -> {t}: {e:?}"));
            assert!(std::fs::metadata(&out).unwrap().len() > 1000, "{src} -> {t}");
            let again = convert_media(&cfg(), &out, back, ConvertOptions { out_dir: Some(d.join("o2")), ..Default::default() }, progress.clone()).await.unwrap_or_else(|e| panic!(".{t} -> {back}: {e:?}"));
            assert!(std::fs::metadata(&again).unwrap().len() > 1000, ".{t} -> {back}");
        }
    }
}

/// 3D formats added on top of the backend's list, run through the real assimp (obj/dae/fbx/3ds/x3d/dxf).
#[tokio::test]
async fn extra_3d_formats_round_trip_through_assimp() {
    if !have("assimp") { eprintln!("SKIP: assimp not installed"); return; }
    let d = tmp("3d");
    let cube = "o cube\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 1 1 1\nv 0 1 1\nf 1 2 3 4\nf 5 8 7 6\nf 1 5 6 2\nf 2 6 7 3\nf 3 7 8 4\nf 4 8 5 1\n";
    std::fs::write(d.join("cube.obj"), cube).unwrap();
    let mut made = vec![];
    for t in ["dae", "fbx", "3ds", "x3d"] {
        let out = convert_file(&cfg(), &d.join("cube.obj"), t, ConvertOptions { out_dir: Some(d.join("o")), ..Default::default() }).await.unwrap_or_else(|e| panic!("obj -> {t}: {e:?}"));
        assert!(std::fs::metadata(&out[0]).unwrap().len() > 100, "obj -> {t}");
        made.push((t, out[0].clone()));
    }
    // And back: every new format is also a source.
    for (t, path) in made {
        let back = convert_file(&cfg(), &path, "stl", ConvertOptions { out_dir: Some(d.join("o2")), ..Default::default() }).await.unwrap_or_else(|e| panic!(".{t} -> stl: {e:?}"));
        let stl = std::fs::read(&back[0]).unwrap();
        assert!(stl.len() > 200, ".{t} -> stl is a real mesh ({} bytes)", stl.len());
    }
    std::fs::write(d.join("tri.dxf"), "0\nSECTION\n2\nENTITIES\n0\n3DFACE\n8\n0\n10\n0.0\n20\n0.0\n30\n0.0\n11\n1.0\n21\n0.0\n31\n0.0\n12\n1.0\n22\n1.0\n32\n0.0\n13\n0.0\n23\n1.0\n33\n0.0\n0\nENDSEC\n0\nEOF\n").unwrap();
    let out = convert_file(&cfg(), &d.join("tri.dxf"), "obj", ConvertOptions { out_dir: Some(d.join("o3")), ..Default::default() }).await.unwrap_or_else(|e| panic!("dxf -> obj: {e:?}"));
    assert!(std::fs::read_to_string(&out[0]).unwrap().contains("\nf "), "the DXF face became an OBJ face");
}
