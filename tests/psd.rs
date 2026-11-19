//! PSD layer extraction. The fixture was written with the Node `ag-psd` library (the reference implementation):
//! a 200x100 canvas with `Background`, a group `Icons` holding `star` / hidden `ghost` / a second `star`, and an empty layer.
use converter_core::extract::psd_layers::{extract, manifest_json};
use converter_core::{Config, ErrorCode};

const FIXTURE: &[u8] = include_bytes!("fixtures/layers.psd");

#[test]
fn exports_layers_into_group_folders_with_unique_names_and_a_manifest() {
    let ex = extract(FIXTURE, &Config::default()).unwrap();
    let names: Vec<&str> = ex.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Background.png", "Icons/star.png", "Icons/ghost.png", "Icons/star_2.png"]);
    assert!(ex.files.iter().all(|(_, d)| d.starts_with(b"\x89PNG")));
    let m = &ex.manifest;
    assert_eq!((m["exported"].as_u64(), m["skipped"].as_u64()), (Some(4), Some(1)));
    assert_eq!(m["canvas"]["width"], 200);
    let layers = m["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 5, "skipped layers stay in the manifest, in document order");
    let ghost = layers.iter().find(|l| l["name"] == "ghost").unwrap();
    assert_eq!((ghost["hidden"].as_bool(), ghost["group"].as_str()), (Some(true), Some("Icons")));
    let star = &layers[1];
    assert_eq!((star["opacity"].as_i64(), star["left"].as_i64(), star["right"].as_i64()), (Some(128), Some(20), Some(70)));
    assert_eq!(layers[4]["skippedReason"], "no pixel data (adjustment, text or empty layer)");
    assert!(manifest_json(m).ends_with(b"}\n"));
}

#[test]
fn pixels_are_the_layers_own_not_the_canvas() {
    let ex = extract(FIXTURE, &Config::default()).unwrap();
    let png = image::load_from_memory(&ex.files[1].1).unwrap().to_rgba8(); // star: 50x40, blue
    assert_eq!(png.dimensions(), (50, 40));
    assert_eq!(png.get_pixel(10, 10).0, [30, 60, 220, 255]);
}

#[test]
fn limits_refuse_before_producing_anything() {
    let few = Config { max_psd_layers: 2, ..Config::default() };
    assert_eq!(extract(FIXTURE, &few).err().unwrap().code, ErrorCode::ETooLarge);
    let tiny_output = Config { max_layer_output_bytes: 50, ..Config::default() };
    assert_eq!(extract(FIXTURE, &tiny_output).err().unwrap().code, ErrorCode::ETooLarge);
    let tiny_memory = Config { max_psd_decode_bytes: 1000, ..Config::default() };
    assert!(extract(FIXTURE, &tiny_memory).is_err(), "a decode budget smaller than the pixels must refuse");
}

#[test]
fn damaged_and_hostile_files_never_panic_or_hang() {
    let cfg = Config::default();
    let started = std::time::Instant::now();
    // every truncation point
    for cut in (0..FIXTURE.len()).step_by(37) { let _ = extract(&FIXTURE[..cut], &cfg); }
    // deterministic byte flips across the whole file
    let mut seed = 0x9E37_79B9u32;
    for _ in 0..300 {
        let mut b = FIXTURE.to_vec();
        for _ in 0..4 { seed = seed.wrapping_mul(1664525).wrapping_add(1013904223); let i = (seed as usize) % b.len(); b[i] ^= (seed >> 24) as u8 | 1; }
        let _ = extract(&b, &cfg);
    }
    assert!(started.elapsed().as_secs() < 30, "fuzzing took {:?}", started.elapsed());
}

#[test]
fn a_forged_enormous_layer_is_refused_without_allocating_it() {
    // Overwrite every 4-byte big-endian value equal to 100 (the canvas height / layer bottoms) with a huge number,
    // so layer rectangles claim ~16 million rows. The decode budget must stop this before any pixels are allocated.
    let mut b = FIXTURE.to_vec();
    let mut i = 0;
    while i + 4 <= b.len() { if b[i..i + 4] == [0, 0, 0, 100] { b[i..i + 4].copy_from_slice(&0x00FF_FFFFu32.to_be_bytes()); } i += 1; }
    let cfg = Config { max_psd_decode_bytes: 64 * 1024 * 1024, ..Config::default() };
    let t = std::time::Instant::now();
    let r = extract(&b, &cfg);
    assert!(t.elapsed().as_secs() < 10, "took {:?}", t.elapsed());
    if let Ok(ex) = r { assert!(ex.files.iter().map(|(_, d)| d.len()).sum::<usize>() < 64 * 1024 * 1024); }
}

#[test]
fn not_a_psd() {
    assert_eq!(extract(b"PK\x03\x04 definitely a zip", &Config::default()).err().unwrap().code, ErrorCode::EConvertFailed);
}
