// Optional smoke: craft-runtime-smoke.html or ?craftRuntime=1 (redirects to smoke page).
import { connect } from "./craft-runtime-loader.js";

function banner(text, ok) {
  const el = document.getElementById("status") || document.getElementById("loading");
  if (!el) return;
  el.className = ok ? "ok" : "err";
  el.textContent = text;
}

function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
    }),
  ]);
}

export async function runCraftRuntimePilot() {
  const params = new URLSearchParams(location.search);
  const dedicated = /craft-runtime-smoke\.html$/i.test(location.pathname);
  banner(
    dedicated
      ? "craft-runtime smoke: DedicatedWorker at /craft-dedicated-worker.js …"
      : "craft-runtime: SharedWorker at /craft-worker-host.js …",
    true,
  );
  const rt = connect({ dedicated });
  const ready = await withTimeout(
    rt.init({
      app_id: "lightcraft",
      capabilities: ["tier_store"],
    }),
    15000,
    "worker init",
  );
  if (params.has("flush")) {
    await withTimeout(rt.storageFlush(), 15000, "storage_flush");
  }
  const mode = dedicated ? "DedicatedWorker" : "SharedWorker";
  banner(
    `craft-runtime ready (${mode}, web-root worker).` +
      (params.has("flush") ? " storage_flush ok." : " Add ?flush=1 to test OPFS flush."),
    true,
  );
  return ready;
}

const smokePage = /craft-runtime-smoke\.html$/i.test(location.pathname);
const smokeQuery = new URLSearchParams(location.search).has("craftRuntime");
if (smokePage || smokeQuery) {
  runCraftRuntimePilot().catch((e) => {
    banner(`craft-runtime failed:\n${e?.stack || e}`, false);
    console.error(e);
  });
}
