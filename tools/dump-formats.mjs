// One-off: dumps backend/src/formats*.ts tables to core/data/formats.json. Read-only on backend/.
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
const here = dirname(fileURLToPath(import.meta.url));
const be = join(here, '..', '..', 'backend', 'src');
const f = await import(join(be, 'formats.ts'));
const m = await import(join(be, 'formats-media.ts'));
const out = {
  targets: f.TARGETS,
  sources: f.SOURCES,
  media: Object.fromEntries(m.MEDIA_TARGET_IDS.map((id) => [id, m.mediaFormat(id)])),
  markupExtensions: f.MARKUP_EXTENSIONS,
  archiveExtensions: f.ARCHIVE_EXTENSIONS,
  arrowTargets: [...f.ARROW_TARGETS],
  arrowExtensions: [...f.ARROW_EXTENSIONS],
};
writeFileSync(join(here, '..', 'data', 'formats.json'), JSON.stringify(out, null, 1));
console.log('targets', Object.keys(out.targets).length, 'sources', Object.keys(out.sources).length, 'media', Object.keys(out.media).length);
