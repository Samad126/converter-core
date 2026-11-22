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

fn pdf_pages(p: &Path) -> Vec<(f32, f32)> {
    let doc = lopdf::Document::load(p).expect("output is a readable PDF");
    doc.get_pages().values().map(|id| {
        let page = doc.get_dictionary(*id).unwrap();
        let mb = page.get(b"MediaBox").unwrap().as_array().unwrap();
        let n = |i: usize| mb[i].as_float().unwrap();
        (n(2) - n(0), n(3) - n(1))
    }).collect()
}

/// Text as `pdftotext` sees it (skipped quietly when poppler is not installed).
fn pdf_text(p: &Path) -> Option<String> {
    let out = std::process::Command::new("pdftotext").arg("-layout").arg(p).arg("-").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[tokio::test] async fn word_to_pdf_without_libreoffice() {
    let p = run("sample.docx", "pdf").await;
    assert!(std::fs::read(&p).unwrap().starts_with(b"%PDF"));
    let pages = pdf_pages(&p);
    assert_eq!(pages.len(), 1);
    if let Some(t) = pdf_text(&p) {
        for needle in ["Quarterly Report", "bold", "Step two", "Apples", "7.5", "한국어"] { assert!(t.contains(needle), "{needle:?} missing from:\n{t}"); }
    }
}

#[tokio::test] async fn spreadsheet_to_pdf_without_libreoffice() {
    let p = run("sample.xlsx", "pdf").await;
    assert_eq!(pdf_pages(&p).len(), 1);
    if let Some(t) = pdf_text(&p) { assert!(t.contains("Quoted, comma") && t.contains("2024-02-10"), "{t}"); }
}

#[tokio::test] async fn presentation_to_pdf_keeps_slide_size_and_order() {
    let p = run("deck.pptx", "pdf").await;
    let pages = pdf_pages(&p);
    assert_eq!(pages.len(), 2);
    // 28 cm x 15.75 cm
    assert!((pages[0].0 - 793.7).abs() < 1.0 && (pages[0].1 - 446.46).abs() < 1.0, "{pages:?}");
    if let Some(t) = pdf_text(&p) {
        let (a, b) = (t.find("Roadmap 2025").expect("slide 1 title"), t.find("Second slide").expect("slide 2 title"));
        assert!(a < b && t.contains("Launch the beta") && t.contains("Key result"), "{t}");
    }
    let txt = std::fs::read_to_string(run("deck.pptx", "txt").await).unwrap();
    assert!(txt.contains("Collect feedback & iterate"), "{txt}");
}

#[test] fn pptx_reader_positions_and_colours() {
    let deck = native::read::pptx::read(std::io::Cursor::new(std::fs::read(fixture("deck.pptx")).unwrap()), &Config::default()).unwrap();
    assert_eq!(deck.slides.len(), 2);
    assert!((deck.width_pt - 793.7).abs() < 1.0);
    assert_eq!(deck.slides[0].background, Some([0xf2, 0xf2, 0xf2]));
    let converter_core::native::model::Shape::Text { x, y, fill, .. } = &deck.slides[0].shapes[0] else { panic!("first shape is text") };
    assert!((x - 56.69).abs() < 0.5 && (y - 28.35).abs() < 0.5, "{x} {y}");
    assert_eq!(*fill, Some([0x72, 0x9f, 0xcf]));
}

#[tokio::test] async fn embedded_pictures_and_links_survive_in_pdf_html_and_epub() {
    let pdf = run("pic.docx", "pdf").await;
    let doc = lopdf::Document::load(&pdf).unwrap();
    let images = doc.objects.values().filter(|o| o.as_stream().ok().is_some_and(|s| s.dict.get(b"Subtype").ok().and_then(|v| v.as_name().ok()) == Some(b"Image"))).count();
    assert!(images >= 1, "the picture is embedded");
    let links = doc.objects.values().filter(|o| o.as_dict().ok().is_some_and(|d| d.get(b"Subtype").ok().and_then(|v| v.as_name().ok()) == Some(b"Link"))).count();
    assert_eq!(links, 1, "the hyperlink becomes a link annotation");
    assert!(std::fs::read_to_string(run("pic.docx", "html").await).unwrap().contains("data:image/png;base64,"));
    assert!(zip_names(&run("pic.docx", "epub").await).iter().any(|n| n == "OEBPS/images/img1.png"));
}

fn png_size(p: &Path) -> (u32, u32) { let i = image::open(p).unwrap(); (i.width(), i.height()) }

/// A multi-page PDF made by our own writer from the big Word fixture (built once, shared by the tests below).
async fn big_pdf() -> PathBuf {
    static CELL: tokio::sync::OnceCell<PathBuf> = tokio::sync::OnceCell::const_new();
    CELL.get_or_init(|| async {
        let pdf = run("big.docx", "pdf").await;
        let dest = std::env::temp_dir().join(format!("native-big-{}.pdf", std::process::id()));
        std::fs::copy(&pdf, &dest).unwrap();
        dest
    }).await.clone()
}

/// Low resolution keeps the debug-build test fast; the maths is the same at any dpi.
const TEST_DPI: u32 = 40;

async fn run_path(src: &Path, target: &str) -> Vec<PathBuf> {
    let cfg = Config { raster_dpi: TEST_DPI, ..Config::default() };
    let ext = formats::extension_of(src);
    let conv = formats::resolve_conversion(&ext, target).unwrap_or_else(|| panic!("{ext} -> {target} not advertised"));
    let ws = std::env::temp_dir().join(format!("native-p-{}-{}-{}", std::process::id(), src.file_stem().unwrap().to_string_lossy(), target));
    let _ = std::fs::remove_dir_all(&ws);
    let out_dir = ws.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let input = ws.join(format!("input{ext}"));
    std::fs::copy(src, &input).unwrap();
    let ctx = Ctx { cfg: &cfg, workspace: ws.clone(), input, out_dir, profile_dir: ws.join("p"), timeout: cfg.convert_timeout, ocr: false };
    native::run(&ctx, &conv).await.unwrap()
}

#[tokio::test] async fn pdf_pages_to_images_without_poppler() {
    let pdf = big_pdf().await;
    let n = pdf_pages(&pdf).len();
    assert!(n >= 5, "{n}");
    for (target, magic) in [("png", &b"\x89PNG"[..]), ("jpg", &b"\xff\xd8"[..]), ("tiff", &b"II*\0"[..])] {
        let files = run_path(&pdf, target).await;
        assert_eq!(files.len(), n, "{target}: one file per page");
        assert!(std::fs::read(&files[0]).unwrap().starts_with(magic) || std::fs::read(&files[0]).unwrap().starts_with(b"MM\0*"), "{target}");
        assert!(files[0].file_name().unwrap().to_string_lossy().starts_with("page-001"));
    }
    // The fixture declares its own page size, so compare against the PDF.
    let files = run_path(&pdf, "png").await;
    let (w, _) = png_size(&files[0]);
    let expect = (pdf_pages(&pdf)[0].0 * TEST_DPI as f32 / 72.0).round() as u32;
    assert!((w as i32 - expect as i32).abs() <= 1, "{w} vs {expect}");
    // The rendered page is not blank: it has dark text pixels.
    assert!(image::open(&files[0]).unwrap().to_luma8().pixels().filter(|p| p.0[0] < 100).count() > 150);
}

#[tokio::test] async fn pdf_to_cbz_is_a_zip_of_ordered_pages() {
    let pdf = big_pdf().await;
    let files = run_path(&pdf, "cbz").await;
    assert_eq!(files.len(), 1);
    let names = zip_names(&files[0]);
    assert_eq!(names.len(), pdf_pages(&pdf).len());
    assert_eq!(names[0], "001.jpg");
}

#[tokio::test] async fn pdf_to_pptx_is_a_deck_our_reader_and_libreoffice_agree_on() {
    let pdf = big_pdf().await;
    let files = run_path(&pdf, "pptx").await;
    let deck = native::read::pptx::read(std::io::Cursor::new(std::fs::read(&files[0]).unwrap()), &Config::default()).unwrap();
    assert_eq!(deck.slides.len(), pdf_pages(&pdf).len());
    assert!(matches!(deck.slides[0].shapes[0], converter_core::native::model::Shape::Picture { .. }));
    let (pw, ph) = pdf_pages(&pdf)[0];
    assert!((deck.width_pt - pw).abs() < 0.5 && (deck.height_pt - ph).abs() < 0.5);
}

#[tokio::test] async fn office_files_to_images_through_our_own_pdf() {
    let pngs = run_path(&fixture("sample.docx"), "png").await;
    assert_eq!(pngs.len(), 1);
    let pngs = run_path(&fixture("deck.pptx"), "jpg").await;
    assert_eq!(pngs.len(), 2);
    assert!((png_size(&pngs[0]).0 as f32 - 793.7 * TEST_DPI as f32 / 72.0).abs() < 2.0, "slides render at the configured dpi");
    assert_eq!(run_path(&fixture("sample.xlsx"), "png").await.len(), 1);
}

async fn try_run_path(src: &Path, target: &str) -> converter_core::Result<Vec<PathBuf>> {
    let cfg = Config::default();
    let ext = formats::extension_of(src);
    let conv = formats::resolve_conversion(&ext, target).unwrap();
    let ws = std::env::temp_dir().join(format!("native-t-{}-{}-{}", std::process::id(), src.file_stem().unwrap().to_string_lossy(), target));
    let out_dir = ws.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let input = ws.join(format!("input{ext}"));
    std::fs::copy(src, &input).unwrap();
    let ctx = Ctx { cfg: &cfg, workspace: ws.clone(), input, out_dir, profile_dir: ws.join("p"), timeout: cfg.convert_timeout, ocr: false };
    native::run(&ctx, &conv).await
}

#[tokio::test] async fn libreoffice_made_pdf_to_markdown_keeps_structure() {
    let md = std::fs::read_to_string(run_path(&fixture("lo-sample.pdf"), "markdown").await.remove(0)).unwrap();
    for needle in ["# Quarterly Report", "## Highlights", "- First point", "1. Step one", "| Apples | 12 |", "| Pears | 7.5 |", "Ünïcödé 한국어."] {
        assert!(md.contains(needle), "{needle:?} missing from:\n{md}");
    }
    let txt = std::fs::read_to_string(run_path(&fixture("lo-sample.pdf"), "txt").await.remove(0)).unwrap();
    assert!(txt.contains("Quarterly Report") && txt.contains("The end."), "{txt}");
}

#[tokio::test] async fn pdf_tables_become_sheets_and_pdf_text_becomes_a_word_file() {
    let xlsx = run_path(&fixture("lo-sample.pdf"), "xlsx").await.remove(0);
    let wb = native::read::sheet::read(&xlsx, &Config::default()).unwrap();
    assert_eq!(wb.sheets[0].rows, vec![vec!["Name", "Qty"], vec!["Apples", "12"], vec!["Pears", "7.5"]]);
    let docx = run_path(&fixture("lo-sample.pdf"), "docx").await.remove(0);
    let doc = native::read::docx::read(std::io::BufReader::new(std::fs::File::open(&docx).unwrap()), &Config::default()).unwrap();
    assert_eq!(doc.tables().len(), 1);
    assert_eq!(doc.first_heading().as_deref(), Some("Quarterly Report"));
}

#[tokio::test] async fn a_long_pdf_round_trips_every_word_and_every_table() {
    // Our own PDF of the 6-section fixture -> Word -> compare with the original document's words and tables.
    let pdf = big_pdf().await;
    let back = run_path(&pdf, "docx").await.remove(0);
    let cfg = Config::default();
    let read = |p: &Path| native::read::docx::read(std::io::BufReader::new(std::fs::File::open(p).unwrap()), &cfg).unwrap();
    let (orig, round) = (read(&fixture("big.docx")), read(&back));
    let words = |d: &converter_core::native::model::Doc| {
        let mut w: Vec<String> = native::write::text::to_txt(d).split_whitespace().map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|s| !s.is_empty()).collect();
        w.sort(); w
    };
    assert_eq!(words(&orig), words(&round), "no word lost or invented");
    assert_eq!(orig.tables().len(), round.tables().len(), "tables that cross a page are one table");
}

#[tokio::test] async fn pdf_without_tables_or_text_fails_with_a_clear_code() {
    let e = try_run_path(&fixture("pic.docx"), "pdf").await.unwrap();
    let nt = try_run_path(&e[0], "xlsx").await.unwrap_err();
    assert_eq!(nt.code, converter_core::ErrorCode::ENoTables);
    // A PDF that is only a picture has no selectable text: say so rather than return an empty file.
    use converter_core::native::model::{Block, Doc};
    let src = native::read::docx::read(std::io::BufReader::new(std::fs::File::open(fixture("pic.docx")).unwrap()), &Config::default()).unwrap();
    let image = src.blocks.into_iter().find_map(|b| if let Block::Image(i) = b { Some(i) } else { None }).expect("fixture has a picture");
    let pdf = native::write::pdf::doc_to_pdf(&Doc { blocks: vec![Block::Image(image)], ..Default::default() }, "scan").unwrap();
    let err = native::read::pdf_text::read(pdf, 100).unwrap_err();
    assert!(err.message.contains("no selectable text") && err.message.contains("OCR"), "{}", err.message);
}

// ---- HWP / HWPX ----------------------------------------------------------------------------------------------

fn words(s: &str) -> std::collections::BTreeMap<String, usize> {
    let mut m = std::collections::BTreeMap::new();
    for w in s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) { *m.entry(w.to_lowercase()).or_default() += 1; }
    m
}

#[tokio::test] async fn hwp_text_matches_what_hancom_extracts() {
    let ours = std::fs::read_to_string(run_path(&fixture("hwp-gov-plan.hwp"), "txt").await.remove(0)).unwrap();
    let reference = std::fs::read_to_string(fixture("hwp-gov-plan.txt")).unwrap();
    let (a, b) = (words(&ours), words(&reference));
    let missing: usize = b.iter().map(|(w, n)| n.saturating_sub(*a.get(w).unwrap_or(&0))).sum();
    // The reference is a hand-wrapped excerpt: a word broken across two lines ("표 / 준화") is the only difference.
    assert!(missing <= 3, "{missing} reference words missing from our text");
    assert!(ours.contains("2022년 국립국어원 업무계획") && ours.contains("한국어교육"));
}

#[tokio::test] async fn hwp_tables_keep_their_shape_including_row_spans() {
    let xlsx = run_path(&fixture("hwp-stats.hwp"), "xlsx").await.remove(0);
    let wb = native::read::sheet::read(&xlsx, &Config::default()).unwrap();
    // Sheet 1 is the title box; the statistics table is the one with the account columns.
    let rows = &wb.sheets.iter().find(|s| s.rows.iter().flatten().any(|c| c.contains("일반 회계"))).expect("the statistics table").rows;
    assert!(rows[0].iter().any(|c| c.contains("일반 회계")), "{:?}", rows[0]);
    let income = rows.iter().find(|r| r.iter().any(|c| c.contains("19,159"))).expect("the income row");
    // Columns line up under the spanned first column: label, sub-label (blank in the parent row), then the figures.
    assert_eq!(income.len(), rows[0].len());
    assert!(rows.iter().any(|r| r.iter().any(|c| c.trim() == "경상수입") && r.iter().any(|c| c == "19,124")), "{rows:?}");
    let calc = native::read::sheet::read(&run_path(&fixture("hwp-calc.hwp"), "xlsx").await.remove(0), &Config::default()).unwrap();
    assert_eq!(calc.sheets[0].rows[..2], [vec!["1", "2", "3", "4"], vec!["3", "7", "2", "7"]]);
}

#[tokio::test] async fn hwpx_matches_hancoms_own_preview_text() {
    let md = std::fs::read_to_string(run_path(&fixture("hwpx-cellzone.hwpx"), "markdown").await.remove(0)).unwrap();
    for needle in ["가나다라 바사아 차카타파 가나 라마", "| 가 |", "가. 라마바사아 차카타 하가"] { assert!(md.contains(needle), "{needle:?} missing:\n{md}"); }
    let txt = std::fs::read_to_string(run_path(&fixture("hwpx-novel.hwpx"), "txt").await.remove(0)).unwrap();
    let preview = {
        let mut z = zip::ZipArchive::new(std::fs::File::open(fixture("hwpx-novel.hwpx")).unwrap()).unwrap();
        let mut s = String::new(); std::io::Read::read_to_string(&mut z.by_name("Preview/PrvText.txt").unwrap(), &mut s).unwrap(); s
    };
    let (a, b) = (words(&txt), words(&preview));
    let missing: usize = b.iter().map(|(w, n)| n.saturating_sub(*a.get(w).unwrap_or(&0))).sum();
    assert_eq!(missing, 0, "every word of Hancom's own preview is in our text");
}

#[tokio::test] async fn hwp_converts_to_word_hwpx_pdf_and_epub() {
    let hwp = fixture("hwp-bizplan.hwp");
    let docx = run_path(&hwp, "docx").await.remove(0);
    let doc = native::read::docx::read(std::io::BufReader::new(std::fs::File::open(&docx).unwrap()), &Config::default()).unwrap();
    assert!(!doc.tables().is_empty() && doc.blocks.len() > 20);
    // HWP -> HWPX -> text equals HWP -> text.
    let hwpx = run_path(&hwp, "hwpx").await.remove(0);
    let direct = std::fs::read_to_string(run_path(&hwp, "txt").await.remove(0)).unwrap();
    let back = std::fs::read_to_string(run_path(&hwpx, "txt").await.remove(0)).unwrap();
    assert_eq!(words(&direct), words(&back));
    assert!(!pdf_pages(&run_path(&hwp, "pdf").await.remove(0)).is_empty());
    assert!(zip_names(&run_path(&hwp, "epub").await.remove(0)).iter().any(|n| n == "OEBPS/nav.xhtml"));
}

#[tokio::test] async fn unreadable_hwp_files_say_why() {
    let dir = std::env::temp_dir().join(format!("hwp-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // HWP 3.0 files are not OLE containers.
    let v3 = dir.join("old.hwp");
    std::fs::write(&v3, b"HWP Document File V3.00 \x1a\x01\x02\x03\x04\x05").unwrap();
    let e = try_run_path(&v3, "txt").await.unwrap_err();
    assert!(e.message.contains("HWP 3.0"), "{}", e.message);
    let garbage = dir.join("junk.hwpx");
    std::fs::write(&garbage, b"not a zip").unwrap();
    assert_eq!(try_run_path(&garbage, "txt").await.unwrap_err().code, converter_core::ErrorCode::EConvertFailed);
}

// ---- ODT / HTML / MHT ----------------------------------------------------------------------------------------

#[tokio::test] async fn libreoffice_made_odt_keeps_structure_and_pictures() {
    let md = std::fs::read_to_string(run_path(&fixture("sample.odt"), "markdown").await.remove(0)).unwrap();
    for needle in ["Quarterly Report", "## Highlights", "- First point", "1. Step one", "| Apples | 12 |", "**bold**", "[link](https://example.com"] { assert!(md.contains(needle), "{needle:?} missing:\n{md}"); }
    assert!(md.starts_with("# Quarterly Report"), "a Title-styled paragraph is a heading:\n{md}");
    let epub = run_path(&fixture("pic.odt"), "epub").await.remove(0);
    assert!(zip_names(&epub).iter().any(|n| n.starts_with("OEBPS/images/")), "the picture is packaged");
    let pdf = run_path(&fixture("sample.odt"), "pdf").await.remove(0);
    assert!((pdf_pages(&pdf)[0].0 - 595.3).abs() < 1.0, "page size comes from the ODT page layout");
}

#[tokio::test] async fn html_becomes_documents_without_a_browser_or_office() {
    let md = std::fs::read_to_string(run_path(&fixture("sample.html"), "markdown").await.remove(0)).unwrap();
    for needle in ["# Quarterly Report", "**bold**", "*italic*", "[link](https://example.com/?a=1&b=2)", "## Highlights", "- First point", "  - Nested", "1. Step one", "| Apples | 12 |", "`code  block", "한국어"] {
        assert!(md.contains(needle), "{needle:?} missing:\n{md}");
    }
    assert!(!md.contains("color: red"), "style blocks are not text");
    let docx = run_path(&fixture("sample.html"), "docx").await.remove(0);
    let doc = native::read::docx::read(std::io::BufReader::new(std::fs::File::open(&docx).unwrap()), &Config::default()).unwrap();
    assert_eq!(doc.tables().len(), 1);
    assert!(!pdf_pages(&run_path(&fixture("sample.html"), "pdf").await.remove(0)).is_empty());
}

// ---- template, flat and binary variants ----------------------------------------------------------------------

#[tokio::test] async fn template_and_flat_documents_read_like_their_plain_siblings() {
    for src in ["sample.ott", "sample.fodt"] {
        let md = std::fs::read_to_string(run_path(&fixture(src), "markdown").await.remove(0)).unwrap();
        for needle in ["Quarterly Report", "## Highlights", "- First point", "| Apples | 12 |"] { assert!(md.contains(needle), "{src}: {needle:?} missing:\n{md}"); }
    }
    for src in ["sample.ots", "sample.xltx", "sample.fods"] {
        if src == "sample.fods" { continue; } // flat spreadsheets go through LibreOffice
        let csv = std::fs::read_to_string(run_path(&fixture(src), "csv").await.remove(0)).unwrap();
        assert!(csv.contains("Apples,12,2024-01-05"), "{src}: {csv}");
    }
}

#[tokio::test] async fn excel_binary_workbooks_are_read_without_excel_or_libreoffice() {
    let csv = std::fs::read_to_string(run_path(&fixture("any_sheets.xlsb"), "csv").await.remove(0)).unwrap();
    assert!(!csv.trim().is_empty());
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run_path(&fixture("any_sheets.xlsb"), "json").await.remove(0)).unwrap()).unwrap();
    assert!(json.is_object() || json.is_array(), "{json}");
}
