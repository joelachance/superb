// Writes content/docs/ from the Markdown in the repository: README.md,
// the Markdown beside this site in docs/, examples/README.md and
// CONTRIBUTING.md. The Markdown stays the source of truth; this makes it
// MDX (frontmatter from the first heading, `<` and `{` escaped outside
// code, links resolved to site routes) and lays out the sidebar. `pnpm dev`
// and `pnpm build` run it first, and content/docs/ is not committed.
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, posix, relative, resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '../..');
const OUT = resolve(import.meta.dirname, '../content/docs');

const FLOWS = ['session', 'track', 'clip', 'marker', 'transport', 'send', 'midi', 'record', 'metronome', 'export', 'devices'];
const PRIMITIVES = ['buffers', 'realtime', 'transport', 'midi', 'graph', 'dynamics', 'mixer', 'automation', 'analysis', 'undo', 'session', 'plugins', 'io'];

/** [source relative to the repository root, page path under /docs, title override] */
const PAGES = [
  ['README.md', 'index', 'Superb'],
  ['docs/flows.md', 'flows/index', 'Flows'],
  ...FLOWS.map((f) => [`docs/flows/${f}.md`, `flows/${f}`]),
  ['docs/primitives.md', 'primitives/index', 'Primitives'],
  ...PRIMITIVES.map((p) => [`docs/primitives/${p}.md`, `primitives/${p}`]),
  ['docs/ARCHITECTURE.md', 'architecture', 'Architecture'],
  ['docs/TERMINOLOGY.md', 'terminology', 'Terminology'],
  ['examples/README.md', 'examples', 'Examples'],
  ['CONTRIBUTING.md', 'contributing', 'Contributing'],
];

const META = {
  '': { title: 'Superb', pages: ['index', 'flows', 'primitives', 'architecture', 'terminology', 'examples', 'contributing'] },
  flows: { title: 'Flows', defaultOpen: true, pages: ['index', ...FLOWS] },
  primitives: { title: 'Primitives', defaultOpen: true, pages: ['index', ...PRIMITIVES] },
};

const routeOf = new Map(PAGES.map(([src, page]) => [src, '/docs/' + page.replace(/\/?index$/, '')]));

/** Escapes what MDX would read as JSX or an expression, leaving code alone. */
function escapeText(text) {
  return text.replace(/(`+)([\s\S]*?)\1|([<{}])/g, (m, ticks, code, ch) => (ticks ? m : '\\' + ch));
}

function rewriteLinks(text, srcFile) {
  return text.replace(/\]\(([^)\s]+)\)/g, (m, target) => {
    if (/^(https?:|mailto:|#)/.test(target)) return m;
    const [path, anchor] = target.split('#');
    const abs = posix.normalize(posix.join(posix.dirname(srcFile), path));
    const route = routeOf.get(abs);
    if (route) return `](${route}${anchor ? '#' + anchor : ''})`;
    return '](' + target + ')';
  });
}

/** Links to repository files the site does not carry become plain text. */
function unlinkRepoFiles(text) {
  return text.replace(/\[([^\]]+)\]\((?!https?:|mailto:|#|\/)[^)]+\)/g, '$1');
}

function convert(src, title) {
  let md = readFileSync(join(ROOT, src), 'utf8').replace(/<!--[\s\S]*?-->\n?/g, '');
  const lines = md.split('\n');
  let i = 0;
  let heading = title;
  if (lines[0]?.startsWith('# ')) {
    heading ??= lines[0].slice(2).trim();
    i = 1;
  }
  while (i < lines.length && lines[i].trim() === '') i++;
  // A short first paragraph becomes the description under the title.
  let description;
  let j = i;
  while (j < lines.length && lines[j].trim() !== '') j++;
  const para = lines.slice(i, j);
  const text = para.join(' ').trim();
  if (para.length <= 3 && text.length <= 220 && /^[A-Za-z`]/.test(text) && !/^(\||[-*] |\d+\. |#|```)/.test(para[0])) {
    description = text.replace(/`/g, '');
    i = j;
  }
  const body = lines.slice(i).join('\n');
  // Fenced code blocks pass through untouched; everything else is escaped and relinked.
  const parts = body.split(/(^```[\s\S]*?^```[^\n]*$)/m);
  const out = parts.map((part, k) =>
    k % 2
      ? part.replace(/^```(\w+)[^\n]*/, '```$1') // rustdoc's `rust,ignore` is just rust to the highlighter
      : unlinkRepoFiles(rewriteLinks(escapeText(part), src)),
  );
  const front = [`title: ${JSON.stringify(heading ?? 'Untitled')}`];
  if (description) front.push(`description: ${JSON.stringify(description)}`);
  return `---\n${front.join('\n')}\n---\n${out.join('')}`;
}

rmSync(OUT, { recursive: true, force: true });
for (const [src, page, title] of PAGES) {
  const file = join(OUT, page + '.mdx');
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, convert(src, title));
}
for (const [dir, meta] of Object.entries(META)) {
  writeFileSync(join(OUT, dir, 'meta.json'), JSON.stringify(meta, null, 2) + '\n');
}
console.log(`sync-docs: ${PAGES.length} pages written to ${relative(process.cwd(), OUT)}`);
