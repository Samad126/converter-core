# Test fixtures

- `sample.*`, `deck.pptx`, `pic.docx`, `big.docx`, `lo-sample.pdf`: made locally with LibreOffice / this crate.
- `hwp-*.hwp`, `hwpx-*.hwpx`, `hwp-gov-plan.txt`: sample documents from the MIT-licensed
  [edwardkim/rhwp](https://github.com/edwardkim/rhwp) repository (`samples/`), copied unmodified:
  `2022년 국립국어원 업무계획` (a public government document, with its reference text), `biz_plan`, `calc-cell`,
  `2010-01-06` (public statistics), `cellzone-merged-end-fill`, `SO-SUEOP`. They exist so the HWP/HWPX readers are
  tested against files written by Hancom Office, not just against files this crate wrote itself.
