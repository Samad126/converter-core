//! Tunables. Ported from `backend/src/config.ts`; HTTP-only limits are dropped.
use std::path::PathBuf;
use std::time::Duration;

const MB: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Config {
    pub convert_timeout: Duration,
    pub media_timeout: Duration,
    pub sigkill_grace: Duration,
    pub max_concurrent_conversions: usize,
    pub max_concurrent_media: usize,
    pub stale_workspace: Duration,
    pub raster_dpi: u32,
    pub raster_jpeg_quality: u32,
    pub max_raster_pages: usize,
    pub max_table_cells: usize,
    pub max_tables: usize,
    pub max_document_xml_bytes: u64,
    pub max_psd_layers: usize,
    pub max_psd_decode_bytes: u64,
    pub max_layer_output_bytes: u64,
    pub max_archive_entries: usize,
    pub max_archive_uncompressed_bytes: u64,
    pub youtube_max_filesize_bytes: u64,
    pub max_page_operation_files: usize,
    /// Replace an existing output file instead of writing `name (1).ext` beside it.
    pub overwrite_existing: bool,
    /// Per-job temp dirs live here.
    pub temp_root: PathBuf,
    /// Downloaded engines live here (`<engines_dir>/<name>/`).
    pub engines_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let cache = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("converter");
        let data = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("converter");
        Self {
            convert_timeout: Duration::from_secs(300),
            media_timeout: Duration::from_secs(30 * 60),
            sigkill_grace: Duration::from_secs(5),
            max_concurrent_conversions: 2,
            max_concurrent_media: 1,
            stale_workspace: Duration::from_secs(15 * 60),
            raster_dpi: 150,
            raster_jpeg_quality: 90,
            max_raster_pages: 100,
            max_table_cells: 200_000,
            max_tables: 1_000,
            max_document_xml_bytes: 32 * MB,
            max_psd_layers: 500,
            max_psd_decode_bytes: 192 * MB,
            max_layer_output_bytes: 48 * MB,
            max_archive_entries: 5_000,
            max_archive_uncompressed_bytes: 512 * MB,
            youtube_max_filesize_bytes: 200 * MB,
            max_page_operation_files: 20,
            overwrite_existing: false,
            temp_root: cache.join("work"),
            engines_dir: std::env::var_os("CONVERTER_ENGINES_DIR").map(PathBuf::from).unwrap_or_else(|| data.join("engines")),
        }
    }
}
