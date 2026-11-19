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
pandoc, ffmpeg stills, heif, assimp, Calibre, native images, data formats (CSV/TSV/JSON/JSONL/YAML/TOML/INI/XML/SQLite). Not yet: Parquet/ORC/Feather,
PDF->Word/PPT/Excel, archives, email, table/PSD extraction, fonts, media jobs, PDF tools.
