import { getCollection, type CollectionEntry } from 'astro:content';
import { SECTIONS, type Section } from '../content/config';

export interface NavEntry {
  id: string;
  title: string;
  section: Section;
  order: number;
  description: string;
}

// Ordered, flat list of all manual pages (section order from SECTIONS, then
// `order` within section). Drives the sidebar, prev/next pagination, and the
// manual landing index.
export async function manualNav(): Promise<NavEntry[]> {
  const pages = await getCollection('manual');
  return pages
    .map((p: CollectionEntry<'manual'>) => ({
      id: p.id,
      title: p.data.title,
      section: p.data.section,
      order: p.data.order,
      description: p.data.description,
    }))
    .sort((a, b) => {
      const sa = SECTIONS.indexOf(a.section);
      const sb = SECTIONS.indexOf(b.section);
      return sa !== sb ? sa - sb : a.order - b.order || a.title.localeCompare(b.title);
    });
}

export interface NavGroup {
  section: Section;
  entries: NavEntry[];
}

export async function manualGroups(): Promise<NavGroup[]> {
  const nav = await manualNav();
  const groups = new Map<Section, NavEntry[]>();
  for (const e of nav) {
    if (!groups.has(e.section)) groups.set(e.section, []);
    groups.get(e.section)!.push(e);
  }
  return SECTIONS.filter((s) => groups.has(s)).map((section) => ({
    section,
    entries: groups.get(section)!,
  }));
}

// Prev/next for the current page id, relative to the flat ordered nav.
export async function prevNext(
  currentId: string,
): Promise<{ prev?: NavEntry | undefined; next?: NavEntry | undefined }> {
  const nav = await manualNav();
  const i = nav.findIndex((e) => e.id === currentId);
  if (i === -1) return {};
  return { prev: nav[i - 1], next: nav[i + 1] };
}
