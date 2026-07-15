import { describe, expect, test } from 'bun:test';
import { docsChannel, docsPath } from './docs-path';

describe('docsChannel', () => {
  test.each([
    ['/docs/', 'stable'],
    ['/ja/docs/install/', 'stable'],
    ['/zh-cn/docs/', 'stable'],
    ['/docs/preview/', 'preview'],
    ['/ja/docs/preview/install/', 'preview'],
    ['/zh-cn/docs/preview/', 'preview'],
  ])('maps %s to %s', (pathname, expected) => {
    expect(docsChannel(pathname)).toBe(expected);
  });
});

describe('docsPath', () => {
  test.each([
    ['index.mdx', 'docs'],
    ['install.mdx', 'docs/install'],
    ['ja/index.mdx', 'ja/docs'],
    ['ja/install.mdx', 'ja/docs/install'],
    ['zh-cn/install.mdx', 'zh-cn/docs/install'],
    ['preview/index.mdx', 'docs/preview'],
    ['preview/install.mdx', 'docs/preview/install'],
    ['preview/ja/index.mdx', 'ja/docs/preview'],
    ['preview/ja/install.mdx', 'ja/docs/preview/install'],
    ['preview/zh-cn/install.mdx', 'zh-cn/docs/preview/install'],
  ])('maps %s to %s', (entry, expected) => {
    expect(docsPath({ entry })).toBe(expected);
  });
});
