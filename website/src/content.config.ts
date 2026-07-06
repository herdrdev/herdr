import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

const docsLocales = ['ja', 'zh-cn'];

function docsPath({ entry }: { entry: string }) {
  const slug = entry.replace(/\.(md|mdx|markdown|mdown|mkdn|mkd|mdwn)$/i, '');
  const normalized = slug.replace(/\/index$/, '');
  for (const locale of docsLocales) {
    if (normalized === locale) return `${locale}/docs`;
    if (normalized.startsWith(`${locale}/`)) {
      return `${locale}/docs/${normalized.slice(locale.length + 1)}`;
    }
  }
  return normalized === 'index' ? 'docs' : `docs/${normalized}`;
}

export const collections = {
  docs: defineCollection({ loader: docsLoader({ generateId: docsPath }), schema: docsSchema() }),
  blog: defineCollection({
    loader: glob({ pattern: '*.md', base: './src/content/blog' }),
    schema: z.object({
      title: z.string(),
      description: z.string(),
      date: z.coerce.date(),
      draft: z.boolean().default(false),
      ogImage: z.string().optional(),
    }),
  }),
};
