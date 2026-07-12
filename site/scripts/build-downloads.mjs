// build-downloads.mjs — derive the downloadable artifacts from the manual
// source (single source of truth = src/content/manual/*.md). Three outputs:
//   1. dist/manual/<id>.md   per-page Markdown download (frontmatter stripped)
//   2. dist/download/onote-manual.md  whole manual, sidebar-ordered, stripped
//   3. dist/download/onote-manual.zip the per-page .md files zipped
// Then it lints dist/**/*.html for root-absolute internal links
// (href="/manual…" missing the /onote base) and FAILS the build if any slip
// through — the #1 GitHub-Pages subpath footgun.
import { readFile, writeFile, mkdir, readdir, rm } from 'node:fs/promises';
import { createWriteStream } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import archiver from 'archiver';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const SRC = join(ROOT, 'src/content/manual');
const DIST = join(ROOT, 'dist');
const DL = join(DIST, 'download');

// Sidebar section order — MUST match src/content/config.ts SECTIONS.
const SECTIONS = ['Get Started', 'Editor', 'Features', 'Configure', 'Reference', 'Project'];

function parseFrontmatter(src) {
  const m = src.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?/);
  if (!m) return { data: {}, body: src };
  const data = {};
  for (const line of m[1].split(/\r?\n/)) {
    const mm = line.match(/^(\w+):\s*(.*)$/);
    if (mm) data[mm[1]] = mm[2].replace(/^["']|["']$/g, '').trim();
  }
  return { data, body: src.slice(m[0].length).replace(/^\r?\n/, '') };
}

async function readPages() {
  let files;
  try {
    files = (await readdir(SRC)).filter((f) => f.endsWith('.md')).sort();
  } catch (e) {
    if (e.code === 'ENOENT') return []; // no pages yet — caller no-ops
    throw e;
  }
  const pages = [];
  for (const f of files) {
    const id = f.replace(/\.md$/, '');
    const src = await readFile(join(SRC, f), 'utf8');
    const { data, body } = parseFrontmatter(src);
    pages.push({
      id,
      body,
      title: data.title || id,
      section: data.section || 'Reference',
      order: Number(data.order ?? 100),
    });
  }
  return pages.sort((a, b) => {
    const sa = SECTIONS.indexOf(a.section);
    const sb = SECTIONS.indexOf(b.section);
    if (sa !== sb) return (sa < 0 ? 99 : sa) - (sb < 0 ? 99 : sb);
    return a.order - b.order || a.title.localeCompare(b.title);
  });
}

async function lintBasePath() {
  // Fail on root-absolute internal links missing the /onote base.
  const offenders = [];
  async function walk(dir) {
    let entries;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      const p = join(dir, e.name);
      if (e.isDirectory()) await walk(p);
      else if (e.name.endsWith('.html')) {
        const html = await readFile(p, 'utf8');
        const re = /(?:href|src)="(\/(?:manual|download|img)[^"]*)"/g;
        let m;
        while ((m = re.exec(html))) offenders.push(`${p}: ${m[1]}`);
      }
    }
  }
  await walk(DIST);
  if (offenders.length) {
    console.error('\nbuild-downloads: root-absolute internal links found (missing /onote base):');
    for (const o of offenders) console.error('  ' + o);
    console.error('  Use relative links in Markdown (./editor.md) or import.meta.env.BASE_URL.');
    process.exit(1);
  }
}

async function main() {
  await mkdir(join(DIST, 'manual'), { recursive: true });
  await mkdir(DL, { recursive: true });
  const pages = await readPages();

  if (pages.length) {
    // 1. Per-page Markdown (stripped).
    for (const p of pages) {
      await writeFile(join(DIST, 'manual', `${p.id}.md`), p.body + '\n', 'utf8');
    }

    // 2. Whole-manual concatenation (sidebar-ordered). Each page body already
    // opens with its own H1, so we join with a thematic break (---) only — no
    // injected title (the Typst cover titles the PDF; per-page H1s title the .md).
    const concat = pages.map((p) => p.body).join('\n\n---\n\n');
    await writeFile(join(DL, 'onote-manual.md'), concat + '\n', 'utf8');

    // 3. Zip the per-page files.
    await new Promise((resolve, reject) => {
      const out = createWriteStream(join(DL, 'onote-manual.zip'));
      const zip = archiver('zip', { zlib: { level: 9 } });
      out.on('close', resolve);
      out.on('error', reject);
      zip.on('error', reject);
      zip.pipe(out);
      for (const p of pages) {
        zip.append(p.body + '\n', { name: `${p.id}.md` });
      }
      void zip.finalize().then(resolve).catch(reject);
    });

    console.log(
      `build-downloads: ${pages.length} pages → manual/*.md, download/onote-manual.{md,zip}`,
    );
  } else {
    console.warn('build-downloads: no manual pages found — skipping download artifacts.');
  }

  // 4. Lint AFTER all HTML exists (always — catches base-path regressions early).
  await lintBasePath();
}

// If invoked twice in one build (it isn't, but be safe), stale per-page .md
// from a removed source page should not linger. Best-effort: no-op here since
// Astro already wiped dist/.
await rm(join(DL, '.gitkeep'), { force: true });
await main();
