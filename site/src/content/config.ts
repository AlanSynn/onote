import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

// Manual content collection (Astro 5 Content Layer). One Markdown file per page
// under src/content/manual/*.md, loaded by the glob loader → entries keyed by
// `id` (= filename without extension; flat dir → "editor", "install", …).
//
// The Zod schema drives the sidebar nav:
//   - `section` (closed enum) groups pages; a typo fails loudly at build time
//     rather than silently misfiling a page in the nav.
//   - `order` sorts within a section.
//   - one file per page → parallel agents author disjoint files → zero merge
//     conflicts.
//
// SECTIONS defines sidebar group order (single source of truth; also consumed
// by scripts/build-downloads.mjs for PDF/download ordering).
export const SECTIONS = [
  'Get Started',
  'Editor',
  'Features',
  'Configure',
  'Reference',
  'Project',
] as const;

export type Section = (typeof SECTIONS)[number];

const manual = defineCollection({
  loader: glob({ pattern: '**/*.md', base: './src/content/manual' }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    section: z.enum(SECTIONS),
    order: z.number().default(100),
  }),
});

export const collections = { manual };
