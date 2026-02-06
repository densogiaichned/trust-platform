# Gap Analysis: What's Missing for Real Adoption of truST

## Key Correction: Standard Library is Actually Complete
All IEC 61131-3 standard functions (Tables 22-46) are implemented: math (SQRT, SIN, COS, LN, EXP...), bit ops (ROL, ROR, SHL, SHR), selection (MUX, LIMIT, SEL), comparison, string, time/date, and all standard FBs (timers, counters, edge triggers, bistable). This is NOT a gap.

## What Already Works Well
- Complete IEC 61131-3 standard library (114+ functions)
- Full LSP with 33+ features including refactoring
- Production-grade debugger with breakpoints, stepping, variable inspection
- IO Panel with force/release, dual-mode (local/remote)
- Hot reload
- Auto-generated CONFIGURATION files on F5
- Auto-seeded runtime control endpoint defaults
- Two complete example projects (plant_demo, filling_line)

---

## Actual Gaps (Revised After Deep Analysis)

### Gap 1: "New Project" Command (HIGH IMPACT, MEDIUM EFFORT)
**Problem:** No way to create a new project from VS Code. Users must manually:
- Create a folder and `src/` subdirectory
- Create their first `.st` file from scratch (not knowing the syntax)
- Optionally create `trust-lsp.toml`

**Good news:** The extension already auto-generates CONFIGURATION on F5 and seeds runtime defaults. So the gap is smaller than expected - it's really just about creating the initial folder + first `.st` file.

**Solution:** Add a VS Code command "Structured Text: New Project" that:
1. Prompts for project name and location
2. Creates `src/Main.st` with a starter PROGRAM
3. Creates `trust-lsp.toml` with `include_paths = ["src"]`
4. Opens the workspace

### Gap 2: Beginner Examples & Learning Path (HIGH IMPACT, LOW EFFORT)
**Problem:** Only 2 examples exist, both medium-complex multi-file projects. No gentle ramp.
**Solution:** Create 5-8 single-file examples with increasing complexity:
- `01_hello_counter.st` - Bare minimum PROGRAM with a counter
- `02_blinker.st` - BOOL toggle with timer (TON)
- `03_traffic_light.st` - CASE state machine
- `04_tank_level.st` - Analog control with LIMIT
- `05_motor_starter.st` - FUNCTION_BLOCK usage (SR, TON, edge triggers)
- `06_recipe_manager.st` - STRUCT, ARRAY, ENUM
- `07_pid_loop.st` - Real-world control pattern
- `08_conveyor_system.st` - Multiple FBs interacting

### Gap 3: Better Error Messages for Learners (HIGH IMPACT, MEDIUM EFFORT)
**Problem:** Error messages are minimal: `"cannot resolve type 'INTT'"` with no hint.
**Current state:** Levenshtein distance exists in runtime CLI but NOT in the LSP/IDE layer.
**Solution:**
- Add "did you mean?" suggestions to undefined symbol/type errors (E101, E102)
- Add hints for common mistakes from other languages:
  - `=` used as assignment → "Did you mean `:=`?"
  - `==` used for comparison → "In ST, use `=` for comparison"
  - `{`/`}` used → "ST uses `END_PROGRAM`, `END_IF`, etc."
  - `&&`/`||` used → "In ST, use `AND`/`OR`"
- Add context to type mismatch errors: "Expected REAL, got INT. Use `INT_TO_REAL()` to convert."

### Gap 4: VS Code Snippets with Documentation (MEDIUM IMPACT, LOW EFFORT)
**Problem:** LSP completions scaffold constructs but lack:
- Documentation explaining what each construct does
- Meaningful placeholder names (uses `${1:Name}` instead of `${1:MyPump}`)
- No common pattern templates (timer usage, state machine, FB instantiation)

**Solution:**
- Add `.code-snippets` file to the VS Code extension with 15-20 documented snippets
- Include snippets for common patterns, not just language constructs:
  - `ton-usage` → Complete TON timer pattern with enable/done check
  - `state-machine` → CASE-based state machine skeleton
  - `fb-template` → Function block with typical Init/Execute pattern
  - `for-loop` → FOR with meaningful variable names
  - `edge-detect` → R_TRIG/F_TRIG usage pattern


### Gap 5: Import from Other PLC Environments (MEDIUM IMPACT, HIGH EFFORT)
**Problem:** No PLCopen XML import for migrating from CODESYS/TwinCAT/Siemens.
**Assessment:** Important for adoption by existing PLC programmers, but high effort. Can be deferred.

### Gap 6: Additional Communication Protocols (LOW-MEDIUM IMPACT, HIGH EFFORT)
**Problem:** Only GPIO and Modbus/TCP. Missing OPC UA, MQTT, EtherNet/IP.
**Assessment:** Only matters for deployment to real hardware. Simulation mode works fine without these.

---

## Recommended Priority Order

### Phase 1: "Zero to Running in 2 Minutes" (do first)
1. **New Project command** in VS Code
2. **5-8 beginner examples** with learning progression
3. **VS Code snippets** with documentation

### Phase 2: "Learner-Friendly Errors" (do second)
4. **"Did you mean?" suggestions** for undefined symbols
5. **Common mistake detection** for developers from other languages
6. **Conversion hints** in type mismatch errors

### Phase 3: "PLC Pro Features" (do later)
7. PLCopen XML import
8. OPC UA / MQTT protocols
9. HMI/visualization integration

---

## Files to Modify

### Phase 1
- `editors/vscode/package.json` - Add "New Project" command, snippet contribution
- `editors/vscode/src/extension.ts` - Implement New Project command handler
- `editors/vscode/snippets/st.code-snippets` - NEW: snippet definitions
- `examples/tutorials/` - NEW: beginner example files

### Phase 2
- `crates/trust-hir/src/diagnostics.rs` - Enhanced error messages
- `crates/trust-ide/src/diagnostics.rs` - "Did you mean?" logic with Levenshtein
- `crates/trust-ide/src/completion.rs` - Better placeholder names in snippets

### Phase 3
- `crates/trust-syntax/src/parser/` - SFC parsing
- `crates/trust-hir/` - SFC semantic analysis
- `crates/trust-runtime/` - SFC execution + new protocol drivers
