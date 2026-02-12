# Browser Analysis WASM Spike (Internal Prototype)

This directory contains prototype browser host assets for Deliverable 10.

It is intentionally kept outside `examples/` because it targets internal spike
validation, not end-user tutorial workflows.

## Files

- `web/index.html`
- `web/main.js`
- `web/worker.js`

## Build and Run

```bash
scripts/build_browser_analysis_wasm_spike.sh
python3 -m http.server 4173 --directory target/browser-analysis-wasm/web
```

Open `http://127.0.0.1:4173/`.
