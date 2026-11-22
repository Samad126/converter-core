# Test fixtures

- `sample.*`, `deck.pptx`, `pic.docx`, `big.docx`, `lo-sample.pdf`: made locally with LibreOffice / this crate.
- `hwp-*.hwp`, `hwpx-*.hwpx`, `hwp-gov-plan.txt`: sample documents from the MIT-licensed
  [edwardkim/rhwp](https://github.com/edwardkim/rhwp) repository (`samples/`), copied unmodified:
  `2022년 국립국어원 업무계획` (a public government document, with its reference text), `biz_plan`, `calc-cell`,
  `2010-01-06` (public statistics), `cellzone-merged-end-fill`, `SO-SUEOP`. They exist so the HWP/HWPX readers are
  tested against files written by Hancom Office, not just against files this crate wrote itself.
- `any_sheets.xlsb`: from the MIT-licensed [tafia/calamine](https://github.com/tafia/calamine) tests (an Excel-written binary workbook).
- `wp6.wpd`, `works.wps`, `pages4.pages`, `visio.vsdx`, `visio.vsd`: LibreOffice's own writerperfect test files
  (MPL-2.0, `writerperfect/qa/unit/data/`). Each was converted to PDF with a real LibreOffice before being listed as a
  supported source. `.numbers` and `.key` are NOT listed: no sample could be obtained to verify them.
- `sample.ott/.fodt/.ots/.xltx/.fods`, `deck.otp`: exported from the other fixtures by LibreOffice.
