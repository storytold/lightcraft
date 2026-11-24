// Run: node --experimental-strip-types src/components/download-detect.test.ts
import { detect, type Env } from './download-detect.ts';
const base: Env = { ua: '', platform: '', maxTouchPoints: 0, renderer: '', arch: '' };
const cases: [Partial<Env>, string | null][] = [
  [{ ua: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130', platform: 'Windows', arch: 'x86' }, 'win-x64'],
  [{ ua: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130', platform: 'Windows', arch: 'arm' }, 'win-arm64'],
  [{ ua: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:130) Firefox/130', platform: 'Win32' }, 'win-x64'],
  [{ ua: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/605', platform: 'MacIntel', renderer: 'Apple GPU' }, 'mac-any'],
  [{ ua: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/130', platform: 'macOS', arch: 'arm' }, 'mac-arm'],
  [{ ua: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/130', platform: 'macOS', arch: 'x86' }, 'mac-intel'],
  [{ ua: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:130) Firefox/130', platform: 'MacIntel', renderer: 'Apple M1' }, 'mac-arm'],
  [{ ua: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:130) Firefox/130', platform: 'MacIntel', renderer: 'Intel(R) Iris(TM) Plus Graphics' }, 'mac-intel'],
  [{ ua: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/605', platform: 'MacIntel', maxTouchPoints: 5 }, null],
  [{ ua: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)', platform: 'iPhone' }, null],
  [{ ua: 'Mozilla/5.0 (Linux; Android 15; Pixel 9) Chrome/130 Mobile', platform: 'Linux armv8l' }, 'android'],
  [{ ua: 'Mozilla/5.0 (Android 15; Mobile; rv:130) Gecko Firefox/130', platform: 'Linux aarch64' }, 'android'],
  [{ ua: 'Mozilla/5.0 (X11; Linux x86_64) Chrome/130', platform: 'Linux' }, 'linux-appimage'],
  [{ ua: 'Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:130) Firefox/130', platform: 'Linux x86_64' }, 'linux-appimage'],
  [{ ua: 'Mozilla/5.0 (X11; Linux aarch64) Chrome/130', platform: 'Linux aarch64' }, null],
  [{ ua: 'Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) Chrome/130', platform: 'Chrome OS' }, null],
  [{}, null],
];
let bad = 0;
for (const [p, want] of cases) {
  const got = detect({ ...base, ...p });
  if (got !== want) { bad++; console.error('FAIL', JSON.stringify(p), 'got', got, 'want', want); }
}
console.log(`${cases.length - bad}/${cases.length} passed`);
if (bad) process.exit(1);
