const worker = new Worker("./worker.js", { type: "module" });
const pending = new Map();
let requestSequence = 0;
let lastRequestId = null;

const statusEl = document.getElementById("status");
const uriEl = document.getElementById("document-uri");
const sourceEl = document.getElementById("source");
const lineEl = document.getElementById("line");
const characterEl = document.getElementById("character");
const outputEl = document.getElementById("output");

function setStatus(message) {
  statusEl.textContent = message;
}

function setOutput(value) {
  outputEl.textContent =
    typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

function parseNumber(value) {
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

function currentPosition() {
  return {
    line: parseNumber(lineEl.value),
    character: parseNumber(characterEl.value),
  };
}

function currentDocument() {
  return {
    uri: uriEl.value.trim(),
    text: sourceEl.value,
  };
}

function request(method, params = {}, timeoutMs = 1500) {
  const id = `req-${++requestSequence}`;
  lastRequestId = id;

  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject, method });
    worker.postMessage({ id, method, params, timeoutMs });
  });
}

function cancelLastRequest() {
  if (!lastRequestId) {
    setStatus("No in-flight request to cancel.");
    return;
  }
  worker.postMessage({
    method: "cancel",
    params: { requestId: lastRequestId },
  });
  const inFlight = pending.get(lastRequestId);
  if (inFlight) {
    inFlight.reject(new Error(`request ${lastRequestId} cancelled by UI`));
    pending.delete(lastRequestId);
  }
  setStatus(`Cancellation requested for ${lastRequestId}.`);
}

async function run(method, params) {
  setStatus(`Running ${method}...`);
  try {
    const result = await request(method, params);
    setOutput(result);
    setStatus(`Completed ${method}.`);
  } catch (error) {
    setOutput({ error: String(error?.message ?? error) });
    setStatus(`Failed ${method}.`);
  }
}

document.getElementById("apply").addEventListener("click", async () => {
  await run("applyDocuments", { documents: [currentDocument()] });
});

document.getElementById("diagnostics").addEventListener("click", async () => {
  await run("diagnostics", { uri: uriEl.value.trim() });
});

document.getElementById("hover").addEventListener("click", async () => {
  await run("hover", {
    uri: uriEl.value.trim(),
    position: currentPosition(),
  });
});

document.getElementById("completion").addEventListener("click", async () => {
  await run("completion", {
    uri: uriEl.value.trim(),
    position: currentPosition(),
    limit: 25,
  });
});

document.getElementById("status-btn").addEventListener("click", async () => {
  await run("status");
});

document.getElementById("cancel").addEventListener("click", () => {
  cancelLastRequest();
});

worker.addEventListener("message", (event) => {
  const message = event.data;
  if (!message || typeof message !== "object") {
    return;
  }

  if (message.type === "ready") {
    setStatus("Worker ready. Apply documents to start analysis.");
    return;
  }
  if (message.type === "startup_error") {
    setStatus(`Worker startup error: ${message.error}`);
    setOutput({ startup_error: message.error });
    return;
  }

  if (!message.id || !pending.has(message.id)) {
    return;
  }

  const requestState = pending.get(message.id);
  pending.delete(message.id);

  if (message.error) {
    requestState.reject(
      new Error(
        `${requestState.method}: ${message.error.message || "unknown worker error"}`
      )
    );
    return;
  }

  requestState.resolve(message.result);
});

worker.addEventListener("error", (event) => {
  setStatus(`Worker crashed: ${event.message}`);
  setOutput({ worker_error: event.message });
});
