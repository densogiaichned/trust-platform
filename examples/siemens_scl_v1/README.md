# Siemens SCL v1 Example

This example demonstrates the Siemens SCL compatibility baseline in truST:

- `#`-prefixed local/instance references (for example `#Total`, `#Counter`)
- Siemens vendor formatting profile (`vendor_profile = "siemens"`)
- deterministic diagnostics/formatting behavior for the supported subset

## Files

- `src/Main.st`: edge counter function block and program using `#`-prefixed references
- `trust-lsp.toml`: enables Siemens vendor profile

## Run

```bash
trust-runtime build --project .
```

To inspect Siemens formatting behavior in the editor, open this folder in VS Code with the truST extension and run `Structured Text: Format Document`.
