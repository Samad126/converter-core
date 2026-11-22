//! The built-in converters, exercised directly (so the result does not depend on which engines the machine has).
use converter_core::{formats, native, pipelines::Ctx, Config};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name) }

async fn run(src: &str, target: &str) -> PathBuf {
    let cfg = Config::default();
    let ext = formats::extension_of(Path::new(src));
    let conv = formats::resolve_conversion(&ext, target).unwrap_or_else(|| panic!("{ext} -> {target} not advertised"));
    let ws = std::env::temp_dir().join(format!("native-{}-{}-{}", std::process::id(), src, target));
    let out_dir = ws.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let input = ws.join(format!("input{ext}"));
    std::fs::copy(fixture(src), &input).unwrap();
    let ctx = Ctx { cfg: &cfg, workspace: ws.clone(), input, out_dir, profile_dir: ws.join("p"), timeout: cfg.convert_timeout, ocr: false };
    native::run(&ctx, &conv).await.unwrap().remove(0)
}

fn zip_names(p: &Path) -> Vec<String> {
    let z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    z.file_names().map(String::from).collect()
}

#[tokio::test] async fn docx_to_text_formats() {
    let md = std::fs::read_to_string(run("sample.docx", "markdown").await).unwrap();
    assert!(md.starts_with("# Quarterly Report"), "{md}");
    assert!(md.contains("**bold**") && md.contains("*italic*") && md.contains("[link](https://example.com"), "{md}");
    assert!(md.contains("- First point") && md.contains("1. Step one") && md.contains("| Apples | 12 |"), "{md}");
    assert!(md.contains("한국어"));
    let txt = std::fs::read_to_string(run("sample.docx", "txt").await).unwrap();
    assert!(txt.contains("Quarterly Report") && txt.contains("Apples\t12"), "{txt}");
    let html = std::fs::read_to_string(run("sample.docx", "html").await).unwrap();
    assert!(html.contains("<h1>Quarterly Report</h1>") && html.contains("<table>") && html.contains("<ol>"), "{html}");
}

#[tokio::test] async fn docx_to_epub_is_a_valid_package() {
    let p = run("sample.docx", "epub").await;
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(&bytes[30..38], b"mimetype", "mimetype must be the first stored entry");
    let names = zip_names(&p);
    for n in ["META-INF/container.xml", "OEBPS/content.opf", "OEBPS/nav.xhtml", "OEBPS/chapter1.xhtml"] { assert!(names.iter().any(|x| x == n), "{n} in {names:?}"); }
}

#[tokio::test] async fn spreadsheets_in_every_supported_format_read_the_same() {
    for src in ["sample.xlsx", "sample.xls", "sample.ods"] {
        let csv = std::fs::read_to_string(run(src, "csv").await).unwrap();
        assert!(csv.starts_with("name,qty,when"), "{src}: {csv}");
        assert!(csv.contains("Apples,12,2024-01-05") && csv.contains("\"Quoted, comma\""), "{src}: {csv}");
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run(src, "json").await).unwrap()).unwrap();
        assert_eq!(json[1]["qty"], serde_json::json!(7.5), "{src}");
    }
}

#[tokio::test] async fn spreadsheet_round_trips_through_xlsx_and_ods() {
    let xlsx = run("sample.ods", "xlsx").await;
    assert!(zip_names(&xlsx).iter().any(|n| n == "xl/workbook.xml"));
    let ods = run("sample.xlsx", "ods").await;
    assert!(zip_names(&ods).iter().any(|n| n == "content.xml"));
    // Re-read what we wrote with the same reader.
    let wb = converter_core::native::read::sheet::read(&xlsx, &Config::default()).unwrap();
    assert_eq!(wb.sheets[0].rows[1][..2], ["Apples".to_string(), "12".to_string()]);
    let wb = converter_core::native::read::sheet::read(&ods, &Config::default()).unwrap();
    assert_eq!(wb.sheets[0].rows[2][1], "7.5");
}

#[test] fn every_native_pair_is_advertised_and_resolves() {
    for (ext, t) in native::all_pairs() {
        assert!(formats::targets_for(ext).iter().any(|x| x == t), "{ext} -> {t}");
        assert!(formats::resolve_conversion(ext, t).is_some(), "{ext} -> {t}");
    }
}

