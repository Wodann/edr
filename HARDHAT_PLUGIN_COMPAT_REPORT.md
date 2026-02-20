# Hardhat Plugin Compatibility Report — EDR Tracing Back-Compat

**Date:** 2026-02-20
**EDR branch:** `feat/tracing-back-compat`
**Hardhat branch:** `hh2/tracing-unification`

## Overview

This report validates that the new EDR trace format (unified tracing architecture)
is backward-compatible with the existing Hardhat v2 plugin ecosystem. Testing was
performed at three levels:

1. **Plugin test suites** — each plugin's own tests run against local Hardhat+EDR
2. **Third-party project tests** — real-world projects using stock plugins
3. **Third-party projects with linked plugins** — full local stack (EDR + Hardhat + plugins)

## Critical Finding: `includeCallTraces` Default

During testing, solidity-coverage's test suite revealed that `Response::traces()`
returned empty arrays because `include_call_traces` defaults to `IncludeTraces::None`
in the new architecture. This was fixed in Hardhat commit `c46cf5b` by setting
`includeCallTraces: IncludeTraces.All` in the provider's observability config.

## Plugin Test Suite Results

### hardhat-gas-reporter v2.3.0

| Category | Pass | Fail | Notes |
|----------|------|------|-------|
| Unit tests | 16 | 5 | Failures: missing API keys (CMC/Etherscan) |
| Integration tests | 30 | 7 | Failures: missing API keys + Alchemy token |

**Trace-format failures: 0.** All gas measurement and reporting functionality works.

### solidity-coverage v0.8.17

| Category | Pass | Fail | Notes |
|----------|------|------|-------|
| Unit tests (before fix) | 60 | 75 | All failures: 0% coverage |
| Unit tests (after fix) | 135 | 0 | All recovered |
| Integration tests (before fix) | 19 | 23 | All failures: 0% coverage |
| Integration tests (after fix) | 41 | 1 | 1 pre-existing (block gas limit) |

**Trace-format failures after fix: 0.**

### hardhat-tracer v3.4.0

| Category | Pass | Fail | Notes |
|----------|------|------|-------|
| Tests | 9 | 4 | Failures: 2 missing Alchemy key, 2 stdin bug |

**Trace-format failures: 0.** All trace-exercising tests pass (CALLs, STATICCALLs,
DELEGATECALLs, opcodes, `debug_traceTransaction`).

## Third-Party Project Results (with linked plugins)

### Seaport (best result — full validation)

| Mode | Pass | Fail | Extra |
|------|------|------|-------|
| Tests | 402 | 0 | -- |
| Gas Reporter | 402 | 0 | Real gas data (e.g., ConduitController.createConduit avg 223,418) |
| Coverage | 402 | 0 | 13.79% stmts overall; 95% for zones |

### OpenZeppelin Contracts v5.5.0

| Mode | Pass | Fail | Extra |
|------|------|------|-------|
| Tests (ERC20) | 126 | 0 | -- |
| Gas Reporter | 126 | 0 | Real gas data (approve avg 44,656; transfer avg 40,133) |
| Coverage | -- | -- | Compilation fails (instrumentation bloat, not trace-related) |

### NexusMutual

| Mode | Pass | Fail | Extra |
|------|------|------|-------|
| Tests | 0 | 2 | Gas cap issue (30M > 16.7M cap) |
| Gas Reporter | 0 | 2 | Same gas cap issue |
| Coverage | 2 | 0 | Non-zero data (Pool.sol: 29.77% stmts) |

### Additional projects tested (stock plugins, local Hardhat+EDR)

| Project | Tests | Gas Reporter | Coverage | Trace Errors |
|---------|-------|-------------|----------|-------------|
| Rocket Pool | Subset pass | Works | Works | None |
| Synthetix | 99/99 pass | Works | N/A | None |
| Safe Contracts | 0/58 (gas cap) | N/A | N/A | None |

## Non-Trace Issues Found

1. **EIP-7825 gas cap** (Safe, NexusMutual): Transactions exceeding 16,777,216 gas
   fail. This is a hardfork behavioral change, not a trace issue.
2. **Synthetix function overloading**: Pre-existing EDR limitation. Unrelated to traces.
3. **OpenZeppelin coverage compilation**: solidity-coverage instrumentation bloat
   causes compilation failure on the full OZ codebase. Not trace-related.

## Risk Assessment

**Risk of trace format changes breaking existing Hardhat plugins: LOW**

With the `includeCallTraces: IncludeTraces.All` fix in place, the backward-compatibility
layer in EDR (`Response::traces()`) combined with the Hardhat adapter successfully
preserves the expected trace format for all tested plugins:

- **hardhat-gas-reporter**: Fully functional
- **solidity-coverage**: Fully functional (after fix)
- **hardhat-tracer**: Fully functional
