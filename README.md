# converter-core

Rust port of the converter backend: a library plus a `converter` CLI. No HTTP server.
Converts documents, spreadsheets, presentations, images, ebooks, archives and more by driving
external engines (LibreOffice, pandoc, ffmpeg, Calibre, poppler, ...), which it locates on the
machine and reports as missing (`E_ENGINE_MISSING`) rather than crashing.

```
cargo run --bin converter -- doctor            # which engines are installed
cargo run --bin converter -- formats .docx     # what a source can become
cargo run --bin converter -- convert in.txt pdf [--out DIR]
```

`data/formats.json` is generated from the TypeScript backend's format matrix by
`tools/dump-formats.mjs` (expects the backend checkout at `../backend`).

Status: early. Ported so far: format matrix, engine discovery, LibreOffice (direct + raster),
pandoc, ffmpeg stills, heif, assimp, Calibre. Not yet: PDF->Word/PPT/Excel, archives, data formats,
email, table/PSD extraction, fonts, media jobs, PDF tools.
