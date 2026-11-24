// Pure guess at the visitor's system for the Download page, kept apart from the DOM so it can
// be tested with plain Node (`node --experimental-strip-types download-detect.test.ts`).
// Returns a Download.astro item id, or null when no build fits (iPhone/iPad, ChromeOS, ARM Linux)
// or the system is unknown: the page then just asks the visitor to pick.

export type Env = {
  ua: string;
  platform: string; // userAgentData.platform or navigator.platform
  maxTouchPoints: number;
  renderer: string; // unmasked WebGL renderer, '' when unavailable
  arch: string; // userAgentData high-entropy `architecture` ('arm', 'x86'), '' when unavailable
};

export function detect(e: Env): string | null {
  const ua = e.ua || '';
  const plat = (e.platform || '').toLowerCase();
  const arch = (e.arch || '').toLowerCase();
  if (/android/i.test(ua)) return 'android';
  if (/iphone|ipad|ipod/i.test(ua)) return null;
  if (/cros/i.test(ua) || plat.includes('chrome os')) return null;
  if (plat.includes('win') || /windows/i.test(ua)) {
    // Windows browsers report x64 in the UA even on ARM; only Client Hints tell the truth.
    return arch === 'arm' || /\b(arm64|aarch64)\b/i.test(ua) ? 'win-arm64' : 'win-x64';
  }
  if (plat.includes('mac') || /macintosh/i.test(ua)) {
    if (e.maxTouchPoints > 1) return null; // an iPad asking for the desktop site
    if (arch === 'arm') return 'mac-arm';
    if (arch === 'x86') return 'mac-intel';
    // Chrome/Firefox name the chip ("Apple M2"); Safari masks every Mac as "Apple GPU", so
    // that alone proves nothing and the universal file (runs on both) is the safe pick.
    const r = e.renderer || '';
    if (/\bM\d+\b/.test(r)) return 'mac-arm';
    if (/intel|amd|radeon|nvidia/i.test(r)) return 'mac-intel';
    return 'mac-any';
  }
  if (plat.includes('linux') || /linux|x11/i.test(ua)) {
    if (arch === 'arm' || /aarch64|arm/i.test(plat) || /aarch64|armv/i.test(ua)) return null;
    return 'linux-appimage';
  }
  return null;
}
