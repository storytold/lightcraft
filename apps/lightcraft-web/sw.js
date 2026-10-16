// Offline support (personal fork): keeps a copy of the app's files so LightCraft opens without a
// network once it has been loaded. Network first: online, every file is fetched fresh (the files keep
// their names between versions, so a cache-first worker would pin an old module) and the copy is
// refreshed; offline, the copy answers. The photo library itself lives in the browser's storage
// (OPFS), not here.
const CACHE = "lightcraft-app-v1";
const APP_FILES = [
  "./",
  "./index.html",
  "./worker.js",
  "./lightcraft_web.js",
  "./lightcraft_web_bg.wasm",
  "./manifest.webmanifest",
  "./icon-256.png",
  "./icon-512.png",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE).then(async (cache) => {
      // each file on its own: one missing optional file must not stop the rest
      await Promise.all(APP_FILES.map((url) => cache.add(url).catch(() => {})));
      // the Chinese UI font is optional (present in release builds)
      await cache.add("./lightcraft_zh_hans.otf").catch(() => {});
    }).then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("lightcraft-app-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  event.respondWith(
    fetch(req)
      .then((response) => {
        if (response.ok && response.type === "basic") {
          const copy = response.clone();
          caches.open(CACHE).then((cache) => cache.put(req, copy)).catch(() => {});
        }
        return response;
      })
      .catch(async () => {
        const hit = await caches.match(req, { ignoreSearch: true });
        if (hit) return hit;
        // a navigation to the app's folder: the page itself
        if (req.mode === "navigate") {
          const page = await caches.match("./index.html");
          if (page) return page;
        }
        return new Response("LightCraft is offline and this file was not saved for offline use.", {
          status: 503,
          headers: { "Content-Type": "text/plain; charset=utf-8" },
        });
      }),
  );
});
