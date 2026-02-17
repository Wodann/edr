# Plan: Convert `CallTraceArena` to Old Trace Format (Issue #1288)

## Background

PR #1278 unified the tracing architecture by replacing the old
`TraceCollector`-based `Trace<HaltReasonT>` with `CallTraceArena` from
`revm-inspectors`. The N-API `traces` accessor on `Response` was removed
because it returned `Vec<RawTrace>` backed by the old `Trace` type, which is
no longer populated.

Hardhat 2 depends on the `traces` accessor returning `Vec<RawTrace>`. Since
nothing else uses `TraceCollector`, `BeforeMessage`, `AfterMessage`, `Step`,
or the N-API types (`TracingMessage`, `TracingStep`, `TracingMessageResult`),
we can simplify the N-API bindings to match exactly what Hardhat needs, and
convert directly from `CallTraceArena` to N-API types with no intermediate
Rust representation.

## Hardhat's Minimal Types (the contract)

```typescript
export interface MinimalMessage {
  to?: Address;
  codeAddress?: Address;
  value: bigint;
  data: Uint8Array;
  caller: Address;
  gasLimit: bigint;
  isStaticCall: boolean;
}

export interface MinimalInterpreterStep {
  pc: number;
  depth: number;
  opcode: { name: string };
  stack: bigint[];
  memory?: Uint8Array;
}

export interface MinimalExecResult {
  success: boolean;
  executionGasUsed: bigint;
  contractAddress?: Address;
  reason?: SuccessReason | ExceptionalHalt;
  output?: Buffer;
}

export interface MinimalEVMResult {
  execResult: MinimalExecResult;
}
```

## Approach: Direct conversion, no intermediate Rust types

Convert directly from `CallTraceArena` to N-API types (`TracingMessage`,
`TracingStep`, `TracingMessageResult`). This eliminates the `edr_tracing`
intermediate representation entirely.

`RawTrace` stores the arena and a verbose flag, then the `.trace()` getter
does the DFS traversal producing
`Vec<Either3<TracingMessage, TracingStep, TracingMessageResult>>` on demand.

### N-API types (`crates/edr_napi/src/trace.rs`)

Simplified to match Hardhat's minimal types:

**`TracingMessage`** — matches `MinimalMessage`:
```rust
#[napi(object)]
pub struct TracingMessage {
    pub caller: Uint8Array,
    pub to: Option<Uint8Array>,
    pub code_address: Option<Uint8Array>,
    pub value: BigInt,
    pub data: Uint8Array,
    pub gas_limit: BigInt,
    pub is_static_call: bool,
}
```

**`TracingStep`** — matches `MinimalInterpreterStep`:
```rust
#[napi(object)]
pub struct TracingStep {
    pub pc: u32,
    pub depth: u32,
    pub opcode: TracingOpcode,
    pub stack: Vec<BigInt>,
    pub memory: Option<Uint8Array>,
}

#[napi(object)]
pub struct TracingOpcode {
    pub name: String,
}
```

**`TracingMessageResult`** — matches `MinimalEVMResult`:
```rust
#[napi(object)]
pub struct TracingMessageResult {
    pub exec_result: TracingExecResult,
}

#[napi(object)]
pub struct TracingExecResult {
    pub success: bool,
    pub execution_gas_used: BigInt,
    pub contract_address: Option<Uint8Array>,
    pub reason: Option<Either<SuccessReason, ExceptionalHalt>>,
    pub output: Option<Uint8Array>,
}
```

**`RawTrace`** — holds arena + verbose flag:
```rust
#[napi]
#[derive(Clone)]
pub struct RawTrace {
    arena: CallTraceArena,
    verbose: bool,
}

#[napi]
impl RawTrace {
    #[napi(getter)]
    pub fn trace(&self) -> Vec<Either3<TracingMessage, TracingStep, TracingMessageResult>> {
        // DFS traversal of self.arena, directly producing N-API types
    }
}
```

## Conversion Strategy

DFS traversal of the `CallTraceArena`, emitting N-API types directly as a
flat sequence that mirrors the old `Before`/`Step`/`After` message ordering.

For each arena node:
1. Emit a `TracingMessage` (from `CallTraceNode`)
2. Walk `ordering` entries:
   - `Step(i)` → emit `TracingStep` from `node.trace.steps[i]`
   - `Call(i)` → recurse into `node.children[i]`
   - `Log(_)` → skip
3. Emit a `TracingMessageResult` (from `CallTraceNode`)

### `CallTraceNode` → `TracingMessage`

| TracingMessage field | Source |
|---|---|
| `caller` | `Uint8Array::from(trace.caller)` |
| `to` | `Some(trace.address)` for calls, `None` for creates |
| `code_address` | `Some(trace.address)` for calls, `None` for creates |
| `value` | `trace.value` as `BigInt` |
| `data` | `Uint8Array::from(trace.data)` |
| `gas_limit` | `trace.gas_limit` as `BigInt` |
| `is_static_call` | `trace.kind == CallKind::StaticCall` |

### `CallTraceStep` → `TracingStep`

| TracingStep field | Source |
|---|---|
| `pc` | `step.pc as u32` |
| `depth` | `trace.depth as u32` (from parent node) |
| `opcode` | `TracingOpcode { name: OpCode::name_by_op(step.op.get()) }` |
| `stack` | If verbose: all elements as `Vec<BigInt>`. If non-verbose: `vec![last]` or `vec![]`. |
| `memory` | `step.memory.as_ref().map(\|m\| Uint8Array::from(m.as_bytes()))` |

### `CallTraceNode` → `TracingMessageResult`

| TracingExecResult field | Source |
|---|---|
| `success` | `trace.success` |
| `execution_gas_used` | `trace.gas_used` as `BigInt` |
| `contract_address` | For creates: `Some(trace.address)` if success, else `None`. For calls: `None` |
| `reason` | `SuccessOrHalt` from `trace.status`: `Success(r)` → `Some(A(r))`, `Halt(r)` → `Some(B(r))`, `Revert` → `None` |
| `output` | `Some(Uint8Array::from(trace.output))` |

## Implementation Steps

### Step 1: Change non-verbose `TracingInspectorConfig` to record stack snapshots

Hardhat's `MinimalInterpreterStep.stack` is `bigint[]` (always present). The
old `TraceCollector` always captured at least the top of the stack. The current
non-verbose config uses `StackSnapshotType::None`, so `step.stack == None`.

In `crates/edr_provider/src/observability.rs`, change:
```rust
// Before:
TracingInspectorConfig::default_parity().set_steps(true)
// After:
TracingInspectorConfig::default_parity()
    .set_steps(true)
    .set_stack_snapshots(StackSnapshotType::Full)
```

### Step 2: Rewrite N-API trace types and `RawTrace`

In `crates/edr_napi/src/trace.rs`:
- Simplify `TracingMessage`, `TracingStep`, `TracingMessageResult` as above
- Add `TracingOpcode`, `TracingExecResult`
- Change `RawTrace` to hold `CallTraceArena` + `verbose: bool`
- Implement `.trace()` getter as DFS traversal producing N-API types directly

In `crates/edr_napi/src/result.rs`:
- Remove `ExecutionResult`, `SuccessResult`, `RevertResult`, `HaltResult`,
  `CallOutput`, `CreateOutput` (only used by old `TracingMessageResult`)
- Keep `SuccessReason`, `ExceptionalHalt`

### Step 3: Re-add `traces()` as a function on `Response`

In `crates/edr_napi/src/provider/response.rs`:
```rust
#[napi(catch_unwind)]
pub fn traces(&self) -> Vec<RawTrace> {
    self.inner
        .call_trace_arenas
        .iter()
        .map(|arena| RawTrace::new(arena.clone(), self.inner.verbose))
        .collect()
}
```
Use `#[napi(catch_unwind)]` (function) NOT `#[napi(catch_unwind, getter)]` to
avoid repeated expensive conversions.

Propagate `verbose_raw_tracing` into `edr_napi_core::spec::Response` as a
`verbose: bool` field.

### Step 4: Remove dead code

- `edr_tracing` crate: remove `Trace`, `TraceMessage`, `BeforeMessage`,
  `AfterMessage`, `Step`, `Stack`, `TraceCollector`, and the `Inspector` impl.
  If nothing remains, consider removing the crate entirely.
- `crates/edr_provider/src/debugger.rs` — dead `Debugger` struct
- `crates/edr_solidity/src/nested_tracer.rs` — dead `NestedTracer`

### Step 5: Uncomment and update tests

In `crates/edr_napi/test/provider.ts`:
- Uncomment the "verbose mode" `describe` block (lines ~136-487)
- Uncomment the `assertEqualMemory` helper (lines ~754-764)
- Update tests for changed N-API shapes:
  - `step.opcode` is now `{ name: "PUSH1" }` instead of `"PUSH1"`
  - `step.pc` is `number` instead of `bigint`
  - `step.depth` is `number` instead of `u8`
  - `response.traces` is now `response.traces()` (function, not getter)

### Step 6: Regenerate `index.d.ts`

Ensure `Response` exposes `traces(): Array<RawTrace>` and the type
declarations for the simplified types are correct.

## Performance Note

The conversion runs only when `.traces()` is called (a function, not a getter,
to discourage repeated calls). The new `callTraces()` method is unaffected.
