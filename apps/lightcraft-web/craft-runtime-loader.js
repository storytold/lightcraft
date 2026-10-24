// Pilot: craft-worker-host JSON over SharedWorker (apps) or DedicatedWorker (smoke page).
const CRAFT_HOST_TYPES = new Set([
  "init",
  "storage_put",
  "storage_flush",
  "gpu_jobs",
  "cancel",
]);

function isCraftHostMessage(data) {
  return data && typeof data.type === "string" && CRAFT_HOST_TYPES.has(data.type);
}

function buildClient(port) {
  /** @type {Map<number, { resolve: Function, reject: Function }>} */
  const gpuWaiters = new Map();

  port.onmessage = (ev) => {
    const msg = ev.data;
    if (!msg || typeof msg.type !== "string") return;
    if (msg.type === "gpu_batch_done" && gpuWaiters.has(msg.batch_id)) {
      const w = gpuWaiters.get(msg.batch_id);
      gpuWaiters.delete(msg.batch_id);
      w.resolve(msg);
      return;
    }
    if (msg.type === "error" && msg.message) {
      for (const [, w] of gpuWaiters) {
        w.reject(new Error(msg.message));
      }
      gpuWaiters.clear();
    }
  };

  if (typeof port.start === "function") port.start();

  function postHost(data) {
    if (!isCraftHostMessage(data)) {
      return Promise.reject(new Error("not a craft-worker-host message"));
    }
    port.postMessage(data);
    return Promise.resolve();
  }

  return {
    port,
    postHost,
    init(payload) {
      return new Promise((resolve, reject) => {
        const onReply = (ev) => {
          const msg = ev.data;
          if (msg?.type === "ready") {
            port.removeEventListener("message", onReply);
            resolve(msg);
          } else if (msg?.type === "error") {
            port.removeEventListener("message", onReply);
            reject(new Error(msg.message));
          }
        };
        port.addEventListener("message", onReply);
        port.postMessage({ type: "init", ...payload });
      });
    },
    storagePut(key, bytes) {
      return postHost({ type: "storage_put", key, bytes });
    },
    storageFlush() {
      return new Promise((resolve, reject) => {
        const onReply = (ev) => {
          const msg = ev.data;
          if (msg?.type === "storage_flushed") {
            port.removeEventListener("message", onReply);
            resolve(msg);
          } else if (msg?.type === "error") {
            port.removeEventListener("message", onReply);
            reject(new Error(msg.message));
          }
        };
        port.addEventListener("message", onReply);
        port.postMessage({ type: "storage_flush" });
      });
    },
    gpuJobs(batchId, jobs) {
      return new Promise((resolve, reject) => {
        gpuWaiters.set(batchId, { resolve, reject });
        port.postMessage({ type: "gpu_jobs", batch_id: batchId, jobs });
      });
    },
  };
}

function wrapDedicatedWorker(worker) {
  const port = {
    postMessage: (m) => worker.postMessage(m),
    addEventListener: (type, fn) => worker.addEventListener(type, fn),
    removeEventListener: (type, fn) => worker.removeEventListener(type, fn),
    start: () => {},
    set onmessage(fn) {
      worker.onmessage = fn;
    },
  };
  return port;
}

/**
 * @param {{ workerUrl?: string, dedicated?: boolean }} [opts]
 */
export function connect(opts = {}) {
  const metaUrl =
    typeof document !== "undefined"
      ? document.querySelector('meta[name="craft-runtime-worker"]')?.content
      : undefined;
  const smokePage =
    typeof location !== "undefined" &&
    /craft-runtime-smoke\.html$/i.test(location.pathname);
  const dedicated = opts.dedicated ?? smokePage;

  if (dedicated) {
    const workerUrl = opts.workerUrl ?? "/craft-dedicated-worker.js";
    const worker = new Worker(workerUrl);
    return buildClient(wrapDedicatedWorker(worker));
  }

  const workerUrl = opts.workerUrl ?? metaUrl ?? "/craft-worker-host.js";
  const worker = new SharedWorker(workerUrl, { name: "craft-runtime" });
  return buildClient(worker.port);
}
