import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightImageZoom from 'starlight-image-zoom';
import { readFileSync } from 'node:fs';

// The product name lives in brand.toml only.
const brand = readFileSync(new URL('../../brand.toml', import.meta.url), 'utf8');
const brandValue = (key) => new RegExp(`^${key}\\s*=\\s*"([^"]+)"`, 'm').exec(brand)?.[1];
const app = brandValue('display_name');
if (!app) throw new Error('display_name not found in brand.toml');
const appId = brandValue('app_id');
if (!appId) throw new Error('app_id not found in brand.toml');

// Pages never write the app id literally (`cargo xtask brand check` fails on it); they write
// `<app_id>` and the build puts the brand.toml value in, in text and in link URLs.
function brandTokens() {
  return (tree) => {
    const walk = (node) => {
      if (node.type === 'text') node.value = node.value.replaceAll('<app_id>', appId);
      if (typeof node.url === 'string') node.url = node.url.replaceAll('<app_id>', appId);
      for (const child of node.children ?? []) walk(child);
    };
    walk(tree);
  };
}

// Base path is set by the Pages workflow (MANUAL_BASE); `/` for local dev.
export default defineConfig({
  site: process.env.MANUAL_SITE || 'http://localhost:4321',
  base: process.env.MANUAL_BASE || '/',
  markdown: { remarkPlugins: [brandTokens] },
  integrations: [
    starlight({
      title: `${app} Manual`,
      description: 'A photo library and non-destructive raw developer. The short manual.',
      plugins: [starlightImageZoom()],
      customCss: ['./src/styles/manual.css'],
      sidebar: [
        { label: 'Start here', items: [{ slug: 'start/install' }, { slug: 'start/layouts' }, { slug: 'start/tour' }] },
        { label: 'Library', items: [{ autogenerate: { directory: 'library' } }] },
        { label: 'Develop', items: [{ autogenerate: { directory: 'develop' } }] },
        { label: 'AI', items: [{ autogenerate: { directory: 'ai' } }] },
        { label: 'Output', items: [{ autogenerate: { directory: 'output' } }] },
        { label: 'Sync and share', items: [{ autogenerate: { directory: 'sync' } }] },
        { label: 'Automate', items: [{ autogenerate: { directory: 'automate' } }] },
        { label: 'Platforms', items: [{ autogenerate: { directory: 'platforms' } }] },
        { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
      ],
    }),
  ],
});
