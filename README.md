# converter-core

Rust port of the converter backend: a library plus a `converter` CLI. No HTTP server.
Converts documents, spreadsheets, presentations, images, ebooks, archives and more by driving
external engines (LibreOffice, pandoc, ffmpeg, Calibre, poppler, ...), which it locates on the
machine and reports as missing (`E_ENGINE_MISSING`) rather than crashing.

```
cargo run --bin converter -- doctor            # which engines are installed
cargo run --bin converter -- engines list      # what can be downloaded on this platform
cargo run --bin converter -- engines install pandoc   # pinned URL + SHA-256, no admin rights
cargo run --bin converter -- formats .docx     # what a source can become
cargo run --bin converter -- convert pdf in.txt [--out DIR]   # same argument order as the Node CLI
cargo run --bin converter -- media mp3 song.wav       # progress on stderr
cargo run --bin converter -- youtube mp3 <url> --out DIR
cargo run --bin converter -- pdf extract in.pdf --pages 2,4-6
cargo run --bin converter -- pdf fill-form form.pdf --values values.json [--flatten true]
```

`data/formats.json` is generated from the TypeScript backend's format matrix by
`tools/dump-formats.mjs` (expects the backend checkout at `../backend`).

Status: feature-complete against the Node backend. Ported: format matrix, engine discovery, LibreOffice (direct + raster),
pandoc, ffmpeg stills, heif, assimp, Calibre, native images, audio/video with progress, YouTube download (mp3/mp4 via yt-dlp), PDF->Word/PPT/Excel/Markdown, fonts and Parquet/ORC/Feather (via the bundled Python scripts), Word tables to XLSX, PSD layers to PNGs + manifest (`ag-psd` crate; limits and a panic guard because it parses untrusted files), email (.eml), archives (zip/tar/tar.gz/tar.bz2/tar.zst/7z via 7z, with bomb/traversal/symlink checks), data formats (CSV/TSV/JSON/JSONL/YAML/TOML/INI/XML/SQLite). PDF page tools (merge, split, extract, remove, organize, rotate, crop, watermark, page numbers, scan-to-PDF, edit, sign, form fields, fill-form with flatten, redact, compare, OCR; protect, unlock, repair, compress via qpdf). Everything the Node backend converts is ported.

## Built-in converters (no engine needed)

Everything below runs in Rust. An installed engine (LibreOffice, pandoc, poppler, ffmpeg, 7-Zip, Python) is still
preferred where it gives better fidelity; the built-in code is used when the engine is missing, and for formats
only it handles. `converter formats <.ext>` lists what a file can become.

| Source | Becomes (built in) |
|---|---|
| Word (docx docm dotx dotm), ODT/OTT/FODT, HTML, MHT, HWP, HWPX, e-mail (eml msg mbox) | PDF, PNG/JPG, TXT, Markdown, HTML, EPUB, DOCX, HWPX (and XLSX/ODS/CSV for the tables of HWP/HWPX) |
| Spreadsheets (xlsx xlsm xls xlsb ods xltx xlt ots) | PDF, PNG/JPG, CSV, TSV, JSON, HTML, XLSX, ODS, HWPX |
| Slides (pptx pptm ppsx potx) | PDF, PNG/JPG, TXT, HTML, Markdown, HWPX |
| PDF | PNG/JPG/TIFF (page images), CBZ, PPTX (page pictures), TXT, Markdown, HTML, DOCX, XLSX (tables), HWPX |
| Pictures (png jpg bmp gif webp tiff ico qoi tga dds exr hdr pnm ...) and camera RAW previews | each other, and PDF |
| Subtitles (srt vtt sbv lrc ass ssa), NDJSON, Avro | each other / CSV, JSON, YAML, XML, TOML, INI, SQLite |
| zip cbz tar tgz gz xz lzma lz4 | zip, cbz, tar, tar.gz |
| ttf otf woff eot | woff, ttf, otf (same outline flavour) |

Layout is our own (`native::layout`, fonts bundled: Liberation + DejaVu, system fallback for CJK), so output is close
to, not identical to, LibreOffice's. The HWPX writer follows the structure of Hancom-written files but has not been
opened in Hancom Office. Scanned PDFs need OCR (Tesseract). WOFF2 and TTF<->OTF still need Python; 7z/bz2/zst
archives need 7-Zip; `.numbers`, `.key`, `.cab`, `.chm`, `.xps` are not supported (nothing to verify them against).
`data/formats-extra.json` holds the formats on top of the ones generated from the backend.

Python routes (PDF to Word/PowerPoint/Excel, Parquet/ORC/Feather, fonts, compare, redact) need Python with `pdf2docx pdfplumber python-pptx openpyxl python-docx pymupdf<2 pyarrow fonttools brotli`.
You do not have to set that up yourself: `converter engines install python` downloads a portable CPython and installs the
packages into the engines folder (~750 MB), and that copy is preferred over any system Python.
`CONVERTER_BIN_<TOOL>` (e.g. `CONVERTER_BIN_PYTHON`) points any engine at a specific executable.

Engines are downloaded into the user's data folder (`CONVERTER_ENGINES_DIR` overrides it) from the pinned
list in `data/engines.json` (url, sha256, size per platform). Lookup order: `CONVERTER_BIN_<TOOL>` -> our own
downloaded copy -> `PATH`. Downloaded today: pandoc, qpdf, 7zip, yt-dlp, ffmpeg, and LibreOffice on Windows (MSI, extracted with
`msiexec /a`, no admin rights) and macOS (DMG, copied out with `ditto`), and Calibre on all three (tarball / MSI / DMG). CI installs both
for real and converts a document / an ebook with that copy. Calibre on Linux also needs a few system libraries
(`libegl1 libopengl0 libxcb-cursor0 libxkbcommon0`); the app says so instead of showing a traceback. Not yet
downloadable (use a package manager): LibreOffice on Linux, Tesseract, heif, assimp, Python packages.
