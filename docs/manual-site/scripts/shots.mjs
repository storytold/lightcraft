#!/usr/bin/env node
// Renders every manual screenshot in each layout, headless, from the procedural demo library.
//   node scripts/shots.mjs [--cli path/to/app-cli] [shot-name ...]
// Output: src/assets/shots/<layout>/<shot>.jpg (1280 px wide, compressed).
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const site = resolve(here, '..');
const repo = resolve(site, '../..');
const args = process.argv.slice(2);
let cli = process.env.MANUAL_CLI;
const only = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--cli') cli = args[++i];
  else only.push(args[i]);
}
if (!cli) throw new Error('pass --cli <path to the app-cli binary> or set MANUAL_CLI');

const spec = JSON.parse(readFileSync(join(here, 'shots.json'), 'utf8'));
// Layout config is scripts/shots.json; the shots themselves live one array per area in scripts/shots.d/*.json.
const dir = join(here, 'shots.d');
const all = readdirSync(dir).filter((f) => f.endsWith('.json')).sort().flatMap((f) => JSON.parse(readFileSync(join(dir, f), 'utf8')));
const names = new Set();
for (const s of all) { if (names.has(s.name)) throw new Error(`duplicate shot name: ${s.name}`); names.add(s.name); }
const shots = all.filter((s) => only.length === 0 || only.includes(s.name));
const work = mkdtempSync(join(tmpdir(), 'manual-shots-'));
const settle = { method: 'ui.settle', params: { timeoutMs: 30000 } };

try {
  for (const [layout, cfg] of Object.entries(spec.layouts)) {
    const out = join(site, 'src/assets/shots', layout);
    mkdirSync(out, { recursive: true });
    const vars = cfg.vars ?? {};
    const sub = (v) => typeof v === 'string' ? (vars[v] ?? v) : Array.isArray(v) ? v.map(sub)
      : v && typeof v === 'object' ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, sub(x)])) : v;
    const lines = [...cfg.steps];
    for (const s of shots.filter((x) => !x.layouts || x.layouts.includes(layout))) {
      // A step with `only: [layout, …]` runs in those layouts alone.
      const steps = s.steps.filter((st) => !st.only || st.only.includes(layout)).map(({ only, ...st }) => sub(st));
      lines.push(...steps, settle, { method: 'ui.screenshot', params: { path: join(work, `${layout}-${s.name}.png`) } });
    }
    const script = join(work, `${layout}.jsonl`);
    writeFileSync(script, lines.map((l) => JSON.stringify(l)).join('\n') + '\n');
    // A throw-away settings folder and no shared prefs: the run never reads or writes the real settings,
    // and an empty face-model folder keeps the face scan (and its activity row) out of the shots.
    const env = { ...process.env, ...isolatedEnv(work) };
    execFileSync(cli, ['snapshot', '--demo', '--script', script, '--size', cfg.size], { cwd: repo, env, stdio: 'inherit' });
    for (const s of shots.filter((x) => !x.layouts || x.layouts.includes(layout))) {
      toJpeg(join(work, `${layout}-${s.name}.png`), join(out, `${s.name}.jpg`));
    }
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}

// `<PREFIX>_…` variables: the prefix comes from brand.toml's env_prefix.
function isolatedEnv(dir) {
  const toml = readFileSync(join(repo, 'brand.toml'), 'utf8');
  const prefix = /env_prefix\s*=\s*"([^"]+)"/.exec(toml)?.[1];
  if (!prefix) throw new Error('env_prefix not found in brand.toml');
  const empty = join(dir, 'no-face-models');
  const home = join(dir, 'home');
  mkdirSync(empty, { recursive: true });
  mkdirSync(home, { recursive: true });
  return {
    [`${prefix}_FACE_MODELS`]: empty,
    [`${prefix}_NO_PREFS`]: '1',
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    APPDATA: join(home, 'AppData'),
  };
}

// PNG -> 1280 px wide JPEG: ImageMagick when installed, else ffmpeg.
var converter; // var: toJpeg runs before this line is reached
function toJpeg(png, jpg) {
  converter ??= ['magick', 'ffmpeg'].find((c) => { try { execFileSync(c, ['-version'], { stdio: 'ignore' }); return true; } catch { return false; } });
  if (converter === 'magick') {
    execFileSync('magick', [png, '-resize', '1280x>', '-strip', '-sampling-factor', '4:2:0', '-quality', '82', jpg]);
  } else if (converter === 'ffmpeg') {
    execFileSync('ffmpeg', ['-v', 'error', '-y', '-i', png, '-vf', "scale='min(1280,iw)':-2", '-pix_fmt', 'yuvj420p', '-q:v', '4', '-map_metadata', '-1', jpg]);
  } else {
    throw new Error('install ImageMagick (magick) or ffmpeg to convert the screenshots');
  }
}
