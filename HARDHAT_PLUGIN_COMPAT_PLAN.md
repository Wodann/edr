# Hardhat Plugin Compatibility Testing Plan

## Context

The `feat/tracing-back-compat` branch on `NomicFoundation/edr` adds a `Response::traces()` method
that provides a backwards-compatible raw trace format matching Hardhat's `MinimalMessage`,
`MinimalInterpreterStep`, and `MinimalEVMResult` interfaces. This replaces the previous
`ExecutionResult`-based tracing API with a new flattened `TracingMessage | TracingStep | TracingMessageResult`
format.

The corresponding Hardhat branch (`hh2/tracing-unification`) consumes these traces in
`EdrProviderWrapper` to emit VM events (`beforeMessage`, `step`, `afterMessage`, `beforeTx`, `afterTx`)
for debugging and analysis tools.

**Goal**: Verify that the new EDR tracing API does not break third-party Hardhat plugins that
depend on VM events or tracing infrastructure, particularly `hardhat-gas-reporter` and
`solidity-coverage`.

## Scope

### In Scope
- Third-party Hardhat plugins that listen to VM events emitted by the provider
- Projects from issue #254 that use `hardhat-gas-reporter` and/or `solidity-coverage`
- The patched Hardhat 2.28.4 (via `patches/hardhat@2.28.4.patch`) with locally-built EDR

### Out of Scope
- Hardhat 3 compatibility (Chainlink uses Hardhat 3; this is a separate effort)
- Foundry-only projects (Taiko has switched to Foundry)
- Neptune Mutual (repository is no longer publicly accessible)
- Uniswap v3-core (frozen on Hardhat ^2.2.0 with deprecated plugins; too old to be representative)

## Key API Changes to Validate

The `feat/tracing-back-compat` branch changes these interfaces:

1. **`TracingMessage`**: Removed `depth`, `code` fields. Reordered remaining fields to match
   Hardhat's `MinimalMessage`.
2. **`TracingStep`**: Changed `pc` from `BigInt` to `u32`, `depth` from `u8` to `u32`,
   `opcode` from `String` to `TracingOpcode { name: String }`. Stack is now always full
   (not just top element).
3. **`TracingMessageResult`**: Replaced `executionResult: ExecutionResult` with
   `execResult: TracingExecResult { success, executionGasUsed, contractAddress, reason, output }`.
4. **`Response::traces()`**: New method (was previously commented out) returning
   `Vec<Vec<Either3<TracingMessage, TracingStep, TracingMessageResult>>>`.
5. **Removed types**: `ExecutionResult`, `SuccessResult`, `RevertResult`, `HaltResult`,
   `CallOutput`, `CreateOutput`.

## Testing Strategy

### Phase 1: Build Local EDR + Patched Hardhat

**Steps:**
1. Check out `feat/tracing-back-compat` branch of EDR
2. Build the native EDR NAPI module:
   ```bash
   cd crates/edr_napi
   pnpm run build:tracing  # builds with --release --features op,tracing
   ```
3. Clone Hardhat at `hh2/tracing-unification` branch
4. In the Hardhat clone, override `@nomicfoundation/edr` to point to the locally-built EDR:
   ```bash
   # In hardhat-core's package.json, use a file: or link: reference
   # Or use pnpm overrides in the root package.json
   ```
5. Build Hardhat:
   ```bash
   cd packages/hardhat-core
   pnpm install
   pnpm run build
   ```

### Phase 2: Plugin-Focused Compatibility Testing

For each target project, the test procedure is:

1. Clone the project
2. Install dependencies
3. Replace `hardhat` with the locally-built patched version (via `pnpm overrides` or `npm link`)
4. Replace `@nomicfoundation/edr` with the locally-built version
5. Run the project's test suite
6. Compare results (pass/fail counts) against a baseline run with stock Hardhat

#### Target Projects (Prioritized by Plugin Coverage)

| # | Project | Hardhat Version | Key Plugins | Priority | Notes |
|---|---------|----------------|-------------|----------|-------|
| 1 | OpenZeppelin/openzeppelin-contracts | ^2.28.5 | gas-reporter, solidity-coverage | **High** | Most up-to-date setup; tests both key plugins |
| 2 | safe-global/safe-contracts | ^2.27.0 | hardhat-toolbox (bundles gas-reporter + coverage) | **High** | All tests passed in original testing; good baseline |
| 3 | rocket-pool/rocketpool | 2.22.12 | gas-reporter, solidity-coverage | **High** | All tests passed originally; 4x perf gain expected |
| 4 | NexusMutual/smart-contracts | ^2.26.3 | gas-reporter, coverage, **hardhat-tracer** | **Medium** | hardhat-tracer directly consumes VM traces |
| 5 | ProjectOpenSea/seaport | ^2.21.0 | gas-reporter, coverage | **Medium** | Hybrid Hardhat+Foundry; 1 known test failure |
| 6 | Synthetixio/synthetix | ^2.12.7 | gas-reporter, coverage, truffle5 | **Low** | Older setup; known issues with Smock |

### Phase 3: Detailed Plugin Verification

#### 3a. `hardhat-gas-reporter` (v2.x)

**How it uses traces**: Subscribes to `beforeMessage` and `afterMessage` VM events to track gas
consumption per function call. It reads `message.to`, `message.data` (for function selector),
and `result.execResult.executionGasUsed` from the events.

**What to verify**:
- Gas reports are generated (not empty)
- Gas numbers are reasonable (compare with baseline)
- No crashes or unhandled errors in the reporter

**Test command**:
```bash
REPORT_GAS=true npx hardhat test
```

#### 3b. `solidity-coverage` (v0.8.x)

**How it uses traces**: Instruments Solidity source code and then listens to `step` events
to track which lines/branches were executed. It reads `step.pc`, `step.opcode`, and
`step.stack` from each step event.

**What to verify**:
- Coverage report is generated
- Coverage percentages are non-zero and reasonable
- No crashes during instrumentation or trace collection

**Test command**:
```bash
npx hardhat coverage
```

#### 3c. `hardhat-tracer` (v3.x) - NexusMutual only

**How it uses traces**: Deeply inspects VM traces to display human-readable call trees.
Reads `message.to`, `message.data`, `message.value`, `result.execResult`, and step data.

**What to verify**:
- Trace output renders without errors
- Call tree structure appears correct

### Phase 4: Regression Testing with EDR's Own Test Suite

Run the existing EDR NAPI and hardhat-tests suites against the `feat/tracing-back-compat` branch:

```bash
# EDR NAPI tests
cd crates/edr_napi
pnpm run testNoBuild

# Hardhat integration tests
cd hardhat-tests
pnpm test
```

## Test Execution Matrix

| Project | gas-reporter | solidity-coverage | hardhat-tracer | Baseline Failures |
|---------|-------------|-------------------|----------------|-------------------|
| OpenZeppelin | Test | Test | N/A | 0 expected |
| Safe Contracts | Test (via toolbox) | Test (via toolbox) | N/A | 0 expected |
| Rocket Pool | Test | Test | N/A | 0 expected |
| NexusMutual | Test | Test | Test | Plugin errors possible |
| Seaport | Test | Test | N/A | 1 known (VM private props) |
| Synthetix | Test | Test | N/A | 1 known error |

## Success Criteria

1. **No new test failures** in any project compared to baseline (stock Hardhat 2.28.4)
2. **Gas reports generate correctly** with `hardhat-gas-reporter` (non-empty, reasonable values)
3. **Coverage reports generate correctly** with `solidity-coverage` (non-zero percentages)
4. **EDR's own test suites pass** on `feat/tracing-back-compat`
5. **No runtime errors** related to the trace format changes (e.g., `TypeError: Cannot read property 'executionResult'`)

## Risk Areas

1. **`execResult` vs `executionResult` naming**: The old API used `executionResult` on
   `TracingMessageResult`. The new API uses `execResult`. Any plugin that accesses
   `result.executionResult` directly (instead of through Hardhat's adapter layer) will break.

2. **`opcode` type change**: Changed from `string` to `{ name: string }`. Plugins that do
   `step.opcode === "CALL"` will break; they need `step.opcode.name === "CALL"`.

3. **`pc` type change**: Changed from `BigInt` to `number`. Plugins comparing with `===` against
   BigInt values will get type mismatches.

4. **`depth` type change**: Changed from `u8` to `u32` and removed from `TracingMessage`.

5. **Removed `code` field**: `TracingMessage` no longer has `code`. Plugins that inspect
   contract bytecode from trace messages will not work.

6. **Stack completeness**: Stack is now always complete (all entries), not just the top element.
   This is an improvement but changes the data shape for non-verbose mode.

## Estimated Effort

- Phase 1 (Build): ~1 hour (Rust compilation + Hardhat build)
- Phase 2 (Per project): ~30-60 minutes each (clone, setup, run, analyze)
- Phase 3 (Plugin deep-dive): ~1-2 hours (manual inspection of outputs)
- Phase 4 (Regression): ~30 minutes

**Total**: ~6-8 hours for full matrix execution

## References

- EDR Issue: https://github.com/NomicFoundation/edr/issues/254
- EDR Branch: `feat/tracing-back-compat` on NomicFoundation/edr
- Hardhat Branch: `hh2/tracing-unification` on NomicFoundation/hardhat
- Changeset: "Added `Response::traces` method with a backwards compatible raw trace format"
- Related: https://github.com/NomicFoundation/edr/issues/273 (performance benchmarking)
