//! PDF page tools (port of `pdf.ts` / `pdf-pages.service.ts`), on `lopdf`.
//! Everything works on bytes in → bytes out, like the TS API.
pub mod draw;
pub mod forms;
pub mod overlay;
pub mod pages;
pub mod python_ops;
pub mod qpdf;
pub mod ranges;
mod widths;
