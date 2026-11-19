# converter-core

Rust port of the converter backend: a library plus a `converter` CLI. No HTTP server.
Converts documents, spreadsheets, presentations, images, ebooks, archives and more by driving
external engines (LibreOffice, pandoc, ffmpeg, Calibre, poppler, ...), which it locates on the
machine and reports as missing (`E_ENGINE_MISSING`) rather than crashing.

```
cargo run --bin converter -- doctor            # which engines are installed
cargo run --bin converter -- formats .docx     # what a source can become
cargo run --bin converter -- convert pdf in.txt [--out DIR]   # same argument order as the Node CLI
```

`data/formats.json` is generated from the TypeScript backend's format matrix by
`tools/dump-formats.mjs` (expects the backend checkout at `../backend`).

Status: early. Ported so far: format matrix, engine discovery, LibreOffice (direct + raster),
pandoc, ffmpeg stills, heif, assimp, Calibre, native images, PDF->Word/PPT/Excel/Markdown and fonts (via the bundled Python scripts), Word tables to XLSX, email (.eml), archives (zip/tar/tar.gz/tar.bz2/tar.zst/7z via 7z, with bomb/traversal/symlink checks), data formats (CSV/TSV/JSON/JSONL/YAML/TOML/INI/XML/SQLite). Not yet: Parquet/ORC/Feather,
PSD layer extraction, media jobs, PDF tools.

Python routes need `python3` plus `pdf2docx pdfplumber python-pptx openpyxl python-docx` (PDF) or `fonttools brotli` (fonts).
`CONVERTER_BIN_<TOOL>` (e.g. `CONVERTER_BIN_PYTHON`) points any engine at a specific executable.
