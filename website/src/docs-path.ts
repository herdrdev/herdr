const docsLocales = new Set(['ja', 'zh-cn']);

export function docsChannel(pathname: string) {
  return /^\/(?:ja\/|zh-cn\/)?docs\/preview(?:\/|$)/.test(pathname) ? 'preview' : 'stable';
}

export function docsPath({ entry }: { entry: string }) {
  const slug = entry.replace(/\.(md|mdx|markdown|mdown|mkdn|mkd|mdwn)$/i, '');
  const normalized = slug.replace(/\/index$/, '');
  const segments = normalized.split('/');
  const localeIndex = segments[0] === 'preview' ? 1 : 0;
  const locale = segments[localeIndex];

  if (locale && docsLocales.has(locale)) {
    segments.splice(localeIndex, 1);
    return `${locale}/docs${segments.length > 0 ? `/${segments.join('/')}` : ''}`;
  }

  return normalized === 'index' ? 'docs' : `docs/${normalized}`;
}
