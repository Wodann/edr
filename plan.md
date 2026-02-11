# Plan: Enhanced JSON-RPC Method Documentation

## Goal

Add detailed, structured documentation to every variant of `MethodInvocation` in
`crates/edr_provider/src/requests/methods.rs`. The doc comments will serve as the
source of truth for generating a static documentation website, so consistency in
formatting and naming is paramount.

---

## 1. Documentation Template

Every method variant gets:
1. A **variant-level doc comment** with description, returns, example, and
   implementation details.
2. **Field-level doc comments** on each parameter, which serve as the canonical
   source for parameter documentation.

A CLI tool will extract the field-level docs and the Rust types to generate the
`Params` table automatically (name, type, required/optional, description), so
there is **no need to duplicate a `## Params` table** in the variant-level doc
comment.

```rust
/// # `eth_getBalance`
///
/// Returns the balance of the account at the given address.
///
/// ## Returns
///
/// `QUANTITY` - The balance of the account in wei, hex-encoded.
///
/// ## Example
///
/// **Params:**
///
/// ```json
/// ["0x407d73d8a49eeb85d32cf465507dd71d507100c1", "latest"]
/// ```
///
/// **Result:**
///
/// ```json
/// "0x0234c8a3397aab58"
/// ```
///
/// ## Implementation details
///
/// - The default block parameter is `"latest"` when omitted.
/// - Post-merge block tags (`"safe"`, `"finalized"`) are validated against the
///   current hardfork.
#[serde(rename = "eth_getBalance")]
GetBalance(
    /// `DATA`, 20 bytes - Address to check the balance of.
    Address,
    /// `BlockSpec` - Block number, tag, or EIP-1898 block identifier. Defaults to `"latest"`.
    Option<BlockSpec>,
),
```

### Field-level doc comment conventions

Each field's doc comment becomes a row in the generated Params table. The doc
comment format is:

```
/// `TYPE` - Description of the parameter.
```

where `TYPE` uses the canonical type names from the table below (e.g.
`` `DATA`, 20 bytes ``, `` `QUANTITY` ``, `` `BlockSpec` ``). The CLI tool
derives the columns as follows:

| Column | Source |
|--------|--------|
| **Name** | Inferred from the Rust type or from the JSON-RPC positional index |
| **Type** | Parsed from the backtick-quoted prefix of the field doc comment |
| **Required** | `Option<T>` → No, everything else → Yes |
| **Description** | Everything after the `` `TYPE` - `` prefix |

Default values (e.g. `default = "optional_block_spec::latest"`) should be
mentioned in the description portion (e.g. "Defaults to `\"latest\"`").

### Section rules

| Section | When to include | Notes |
|---------|-----------------|-------|
| **Heading** (`# \`method_name\``) | Always | Exact JSON-RPC method name in backticks |
| **Description** | Always | 1-3 sentences. Functional description, not implementation details |
| **Params** (field-level `///`) | When method has parameters | Written on each enum field. Extracted by CLI tool into a table |
| **Returns** | Always | Type + short description |
| **Example** | Always | `params` array + `result` value only (see §3) |
| **Implementation details** | Only when behavior diverges from standard Ethereum JSON-RPC | Bullet list of deviations/extensions |

### Type naming convention (for Returns and generated Params tables)

Use these canonical type names for consistency across all methods. The CLI tool
maps Rust types to these names when generating the Params table:

| Type name | Meaning |
|-----------|---------|
| `QUANTITY` | Hex-encoded integer (`"0x1a4"`) |
| `DATA` | Hex-encoded bytes (`"0xabcd"`) |
| `DATA`, N bytes | Fixed-length bytes (e.g. `DATA`, 20 bytes for addresses, 32 bytes for hashes) |
| `TAG` | Block tag: `"latest"`, `"earliest"`, `"pending"`, `"safe"`, `"finalized"` |
| `BlockSpec` | `QUANTITY` \| `TAG` \| EIP-1898 block identifier object |
| `Boolean` | JSON boolean |
| `Object` | JSON object (describe fields in a nested table or prose) |
| `Array` | JSON array (describe element type) |

This follows the Ethereum JSON-RPC convention (matching Geth docs) while adding
`BlockSpec` for our EIP-1898 support.

---

## 2. Method Grouping

Methods are already implicitly grouped by their RPC prefix. The doc comments
should reflect this by using a **module-level doc comment** (a `///` block before
the first variant of each group) that introduces the namespace. This provides
structure for the static site generator.

Proposed group separators (as comments within the enum):

```
// ── Standard Ethereum Methods (`eth_*`) ──
// ── Network Methods (`net_*`) ──
// ── Signing Methods (`personal_*`, `eth_sign*`) ──
// ── Web3 Methods (`web3_*`) ──
// ── EVM Methods (`evm_*`) ──
// ── Debug Methods (`debug_*`) ──
// ── Hardhat Methods (`hardhat_*`) ──
```

These section dividers make the enum scannable and map cleanly to navigation
sections on the generated website.

---

## 3. JSON Examples: When and How

### Include params+result examples for ALL methods

Rationale: The target is a documentation website. Users expect to be able to
copy-paste examples. Even for trivial parameterless methods, an example removes
ambiguity.

Since the `method` name is already documented in the heading and the JSON-RPC
envelope (`jsonrpc`, `id`) is boilerplate, examples only show the **`params`
array** and the **`result` value**. The static site generator or CLI tool can
wrap these into full JSON-RPC request/response objects if needed.

### Compact format for parameterless methods

For methods with no parameters (e.g., `eth_accounts`, `eth_blockNumber`,
`eth_chainId`), `Params` is omitted and only the result is shown:

```rust
/// ## Example
///
/// **Result:**
///
/// ```json
/// "0xa"
/// ```
```

### Full multi-line format for methods with complex parameters

For methods like `eth_call`, `eth_getLogs`, `eth_sendTransaction`,
`debug_traceTransaction`, the example should use pretty-printed JSON showing the
full parameter structure:

```rust
/// ## Example
///
/// **Params:**
///
/// ```json
/// [
///   {
///     "to": "0x08b815f80c1c000000000000000000000000000a3",
///     "data": "0x70a08231000000000000000000000000000000000001"
///   },
///   "latest"
/// ]
/// ```
///
/// **Result:**
///
/// ```json
/// "0x00000000000000000000000000000000000000000000003635c9adc5dea00000"
/// ```
```

### Cases where multiple examples add value

Some methods accept **polymorphic parameters** or have **significantly different
behavior depending on inputs**. For these, include 2 examples:

| Method | Why multiple examples help |
|--------|--------------------------|
| `eth_getBlockByNumber` | `hydrated: true` vs `hydrated: false` produces very different response shapes |
| `eth_subscribe` | Different subscription types (`newHeads`, `logs`, `newPendingTransactions`) |
| `hardhat_mine` | With vs without interval parameter |
| `evm_setIntervalMining` | Fixed value vs range array |

Most methods need only **one** example.

---

## 4. Implementation Details to Document

The following implementation details diverge from standard Ethereum JSON-RPC or
Geth and **must** be called out in the "Implementation details" section:

### Default parameter values
- `eth_call`: block defaults to `"latest"` (not all clients do this)
- `eth_estimateGas`: block defaults to `"pending"` (matches Hardhat)
- `eth_getBalance`, `eth_getCode`, `eth_getStorageAt`, `eth_getTransactionCount`:
  block defaults to `"latest"`

### Return value quirks
- `evm_increaseTime`: Returns the total time offset as a **decimal string**
  (not hex-encoded)
- `evm_setNextBlockTimestamp`: Returns the new timestamp as a **decimal string**
- `evm_mine`: Returns `"0"` (string, not number)
- `evm_snapshot`: Returns snapshot ID as `QUANTITY`

### Validation behaviors
- Post-merge block tags (`"safe"`, `"finalized"`) are validated against the
  current hardfork and rejected on pre-merge chains
- `eth_getTransactionByBlockNumberAndIndex`: Does **not** accept EIP-1898 block
  specifications (matches Hardhat behavior)
- `hardhat_setMinGasPrice`: Only works on pre-EIP-1559 hardforks
- `hardhat_setNextBlockBaseFeePerGas`: Only works on EIP-1559+ hardforks
- `hardhat_setStorageAt`: Value must be exactly 32 bytes
- `eth_call`: Supports optional state overrides (third parameter)

### Non-standard methods
- All `evm_*` methods are non-standard (Ganache-compatible)
- All `hardhat_*` methods are Hardhat Network extensions
- `personal_sign`, `eth_signTypedData_v4`: Signing methods operate on local
  accounts managed by the provider

---

## 5. Implementation Steps

### Phase 1: Preparation (research)

1. **Audit each handler** — For every method, read the handler implementation
   to capture:
   - Exact parameter types and defaults
   - Return type and format
   - Validation logic and error conditions
   - Any behavioral quirks

2. **Cross-reference with Geth docs** — For standard `eth_*` methods, verify our
   description matches the canonical Ethereum behavior, then note deviations.

3. **Build example payloads** — Construct realistic JSON examples. For methods
   that operate on state (balances, transactions, etc.), use representative but
   obviously-fake addresses (`0x0000...0001` style) to avoid confusion with real
   mainnet data.

### Phase 2: Documentation writing

Work through the methods in the order they appear in the enum (which already
follows a logical grouping by namespace). For each method:

4. **Write the variant-level doc comment** following the template from §1
5. **Write field-level doc comments** on each parameter field
6. **Add the JSON example** following the rules from §3
7. **Add implementation details** where applicable per §4

Suggested batching (by namespace, to maintain focus and consistency):

| Batch | Methods | Count |
|-------|---------|-------|
| A | `eth_*` standard query methods (accounts, blockNumber, chainId, coinbase, gasPrice, syncing, etc.) | ~10 |
| B | `eth_*` block & transaction retrieval methods | ~10 |
| C | `eth_*` state methods + call/estimateGas | ~8 |
| D | `eth_*` filter/subscription + send methods | ~8 |
| E | `net_*`, `web3_*`, `personal_*`, signing methods | ~5 |
| F | `evm_*` methods | 7 |
| G | `debug_*` methods | 2 |
| H | `hardhat_*` methods | 13 |

### Phase 3: Consistency pass

8. **Verify all doc comments** use the same type names, heading levels, and
   example structure
9. **Verify all field-level doc comments** follow the conventions from §1 and
   include default values where applicable
10. **Fix the two `debug_*` variants** that currently use `//` instead of `///`
    (lines 280, 287 — these are non-doc comments and will be invisible to rustdoc)
11. **Re-order variants** if needed to ensure methods within each namespace are
    alphabetically sorted (they mostly are, but verify)

### Phase 4: Validation

12. **Run `cargo doc`** to verify all doc comments render correctly
13. **Spot-check** that JSON code blocks inside `///` comments are valid JSON
14. **Review** for accuracy against handler implementations

---

## 6. Formatting Rules for Static Site Generation

To ensure the documentation generator can parse these comments reliably:

- **Heading hierarchy**: `# \`method_name\`` (H1), then `## Returns`,
  `## Example`, `## Implementation details` (H2). Never use H3+ inside a
  method's doc comment.
- **Method name in heading**: Always use the exact JSON-RPC method name in
  backtick-quoted code format: `` # `eth_getBalance` ``
- **Tables**: Use standard GitHub-Flavored Markdown pipe tables with header
  separator row
- **Code blocks**: Always use triple-backtick fenced blocks with `json` language
  tag
- **No HTML**: Stick to pure Markdown for portability
- **Line length**: Keep lines in doc comments under 100 characters for readability
  in source
- **Blank lines**: Separate each section with a blank `///` line

---

## 7. Open Questions / Notes

1. **State override documentation**: `eth_call` accepts an optional third
   `StateOverrideOptions` parameter. This is a complex object. I recommend
   documenting it inline with a nested field table rather than cross-referencing a
   separate object definitions page (since we don't have one yet). If the static
   site eventually needs a shared types page, this can be extracted later.

2. **Chain-spec-generic types**: Several parameters are
   `ChainSpecT::RpcCallRequest` / `ChainSpecT::RpcTransactionRequest`. The
   documentation should describe the **L1 Ethereum variant** of these types (the
   standard transaction call object), since that's what users interact with. The
   generic chain spec abstraction is an internal implementation detail.

3. **Error documentation**: Geth and the Ethereum spec both document error codes.
   EDR has a rich error type (`ProviderError` with 60+ variants). Including error
   documentation per-method would be valuable but is a significant additional
   scope. I recommend deferring this to a follow-up and focusing on the happy-path
   documentation first. If desired, a `## Errors` section can be added to the
   template later.
