# Plan: Convert `CallTraceArena` to Old Trace Format (Issue #1288)

## Background

PR #1278 unified the tracing architecture by replacing the old `TraceCollector`-based `Trace<HaltReasonT>` (a flat list of `Before`/`Step`/`After` messages) with `CallTraceArena` from `revm-inspectors` (a tree of `CallTraceNode`s). The N-API `traces` getter on `Response` was commented out because it returned `Vec<RawTrace>` backed by the old `Trace` type, which is no longer populated.

Hardhat 2 depends on the `traces` getter returning `Vec<RawTrace>`, so we need a backwards-compatibility conversion layer.

## What Needs to Happen

Convert each `CallTraceArena` into the old `edr_tracing::Trace<EvmHaltReason>` format, wrap it in `RawTrace`, and re-expose it via the `traces` getter on `Response`.

## Old Format Recap

The old `Trace` is a flat sequence of messages:
```
Before(BeforeMessage)  -- depth 0 (root call)
  Step(Step)           -- each EVM instruction
  Step(Step)
  Before(BeforeMessage) -- depth 1 (subcall)
    Step(Step)
    Step(Step)
  After(AfterMessage)   -- subcall result
  Step(Step)
After(AfterMessage)    -- root call result
```

The `CallTraceArena` is a tree:
- Node 0 (root): `trace.steps[]`, `children[]`, `ordering[]`
- Each child node has its own `trace.steps[]`, `children[]`, etc.

## Conversion Strategy

Perform a depth-first traversal of the arena tree, emitting `Before`/`Step`/`After` messages in the same interleaved order that `TraceCollector` used to produce. The `TraceMemberOrder` enum in each node tells us exactly when subcalls happen relative to steps:

1. For each node, emit a `BeforeMessage`
2. Walk `ordering` entries:
   - `TraceMemberOrder::Step(i)` → emit a `Step` from `node.trace.steps[i]`
   - `TraceMemberOrder::Call(i)` → recurse into `node.children[i]`
   - `TraceMemberOrder::Log(_)` → skip (not part of old format)
3. After processing all ordering entries, emit an `AfterMessage`

### Field Mapping: `CallTraceNode` → `BeforeMessage`

| BeforeMessage field | Source |
|---|---|
| `depth` | `trace.depth` |
| `caller` | `trace.caller` |
| `to` | `Some(trace.address)` for calls, `None` for creates |
| `is_static_call` | `trace.kind == CallKind::StaticCall` |
| `gas_limit` | `trace.gas_limit` |
| `data` | `trace.data.clone()` |
| `value` | `trace.value` |
| `code_address` | `Some(trace.address)` for calls, `None` for creates |
| `code` | `None` (code is not stored in `CallTraceArena`; the old format included it but it's not critical for Hardhat 2's usage — if needed, this can be revisited) |

### Field Mapping: `CallTraceStep` → `Step`

| Step field | Source |
|---|---|
| `pc` | `step.pc as u32` |
| `depth` | parent `trace.depth` as `u64` |
| `opcode` | `step.op.get()` |
| `stack` | `step.stack` → `Stack::Full(vec)` if `Some`, `Stack::Top(None)` if `None` |
| `memory` | `step.memory` → `Some(bytes.to_vec())` if `Some`, `None` if `None` |

### Field Mapping: `CallTraceNode` → `AfterMessage`

| AfterMessage field | Source |
|---|---|
| `execution_result` | Synthesized from `trace.status`, `trace.gas_used`, `trace.output`, `trace.kind` |
| `contract_address` | For creates: `Some(trace.address)` if `trace.success`, else `None`. For calls: `None` |

For `execution_result`, map `trace.status` (an `Option<InstructionResult>`) using `SuccessOrHalt`:
- `Success` → `ExecutionResult::Success { reason, gas_used: trace.gas_used, gas_refunded: 0, logs: vec![], output }`
  - `output`: `Output::Create(trace.output, Some(trace.address))` for creates, `Output::Call(trace.output)` for calls
- `Revert` → `ExecutionResult::Revert { gas_used: trace.gas_used, output: trace.output }`
- `Halt(reason)` → `ExecutionResult::Halt { reason, gas_used: trace.gas_limit }`

Note: `gas_refunded` and `logs` are not available in `CallTraceArena` — use `0` and `vec![]` respectively. This is acceptable because Hardhat 2 only uses traces for stack trace decoding, not for gas accounting.

## Implementation Steps

### Step 1: Add conversion function in `edr_tracing`

Add a new module or function in `crates/tracing/src/lib.rs` (or a new file `crates/tracing/src/convert.rs`):

```rust
pub fn from_call_trace_arena(arena: &CallTraceArena) -> Trace<EvmHaltReason>
```

This function performs the DFS traversal described above.

**Dependencies**: `edr_tracing` will need to depend on `revm-inspectors` (for `CallTraceArena`, `CallTraceNode`, `CallTraceStep`, `TraceMemberOrder`, `CallKind`) and `revm-interpreter` (for `InstructionResult`, `SuccessOrHalt`).

### Step 2: Update `RawTrace` to support construction from `CallTraceArena`

In `crates/edr_napi/src/trace.rs`, add:

```rust
impl RawTrace {
    pub fn from_arena(arena: &CallTraceArena) -> Self {
        let trace = edr_tracing::Trace::from_call_trace_arena(arena);
        Self { inner: Arc::new(trace) }
    }
}
```

### Step 3: Uncomment and update `traces` getter in `Response`

In `crates/edr_napi/src/provider/response.rs`, uncomment the `traces` method:

```rust
#[napi(catch_unwind, getter)]
pub fn traces(&self) -> Vec<RawTrace> {
    self.inner
        .call_trace_arenas
        .iter()
        .map(|arena| RawTrace::from_arena(arena))
        .collect()
}
```

### Step 4: Uncomment and re-enable tests

In `crates/edr_napi/test/provider.ts`:
- Uncomment the "verbose mode" `describe` block (lines ~136-487)
- Uncomment the `assertEqualMemory` helper (lines ~754-764)
- Adjust tests if the converted format differs slightly (e.g. `code` field may be `None`)

### Step 5: Verify and update `index.d.ts`

The TypeScript declaration file (`crates/edr_napi/index.d.ts`) is auto-generated by `napi-rs`. After adding the `traces` getter back, regenerate the type declarations so that `Response` once again exposes:
```typescript
get traces(): Array<RawTrace>
```

## Considerations

1. **`code` field**: The old `BeforeMessage` included the contract bytecode. `CallTraceArena` doesn't store this. For Hardhat 2 compatibility, we should check whether it actually reads the `code` field from `TracingMessage`. If it does, we may need to pass the `address_to_executed_code` map into the conversion. If not, `None` is fine.

2. **`gas_refunded` and `logs`**: These are not available per-node in the arena. Using `0` and `vec![]` should be acceptable since Hardhat 2 uses traces for stack trace analysis, not gas accounting.

3. **Stack representation**: When `verbose_raw_tracing` is false, `TracingInspectorConfig::default_parity().set_steps(true)` records steps but with `StackSnapshotType::None`, meaning `step.stack` will be `None`. The old format would have `Stack::Top(last_element)` in non-verbose mode. We should map `None` → `Stack::Top(None)` and `Some(stack)` → `Stack::Full(vec)` (or `Stack::Top(last)` depending on the snapshot type config).

4. **Performance**: This conversion is lazy (done in the getter), so it only runs when Hardhat 2 actually accesses `.traces`. The new `callTraces()` method remains unaffected.
