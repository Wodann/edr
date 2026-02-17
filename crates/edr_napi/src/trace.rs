// In contrast to the functions in the `#[napi] impl XYZ` block,
// the free functions `#[napi] pub fn` are exported by napi-rs but
// are considered dead code in the (lib test) target.
// For now, we silence the relevant warnings, as we need to mimick
// the original API while we rewrite the stack trace refinement to Rust.
#![cfg_attr(test, allow(dead_code))]

use edr_chain_spec::EvmHaltReason;
use edr_chain_spec_evm::interpreter::{InstructionResult, SuccessOrHalt};
use edr_primitives::bytecode::opcode::OpCode;
use edr_solidity_tests::traces::{CallKind, CallTraceArena, TraceMemberOrder};
use napi::bindgen_prelude::{BigInt, Either, Either3, Uint8Array};
use napi_derive::napi;

use crate::result::{ExceptionalHalt, SuccessReason};

mod library_utils;

mod debug;
mod exit;
mod model;
mod return_data;
pub mod solidity_stack_trace;

/// Matches Hardhat's `MinimalMessage` interface.
#[napi(object)]
pub struct TracingMessage {
    /// Sender address
    #[napi(readonly)]
    pub caller: Uint8Array,

    /// Recipient address. None if it is a Create message.
    #[napi(readonly)]
    pub to: Option<Uint8Array>,

    /// Address of the code that is being executed. Can be different from `to`
    /// if a delegate call is being done.
    #[napi(readonly)]
    pub code_address: Option<Uint8Array>,

    /// Value sent in the message
    #[napi(readonly)]
    pub value: BigInt,

    /// Input data of the message
    #[napi(readonly)]
    pub data: Uint8Array,

    /// Transaction gas limit
    #[napi(readonly)]
    pub gas_limit: BigInt,

    /// Whether it's a static call
    #[napi(readonly)]
    pub is_static_call: bool,
}

/// Matches Hardhat's `MinimalInterpreterStep` interface.
#[napi(object)]
pub struct TracingStep {
    /// The program counter
    #[napi(readonly)]
    pub pc: u32,
    /// Call depth
    #[napi(readonly)]
    pub depth: u32,
    /// The executed opcode
    #[napi(readonly)]
    pub opcode: TracingOpcode,
    /// The entries on the stack.
    #[napi(readonly)]
    pub stack: Vec<BigInt>,
    /// The memory at the step. None if verbose tracing is disabled.
    #[napi(readonly)]
    pub memory: Option<Uint8Array>,
}

/// Opcode information for a tracing step.
#[napi(object)]
pub struct TracingOpcode {
    /// The name of the opcode
    #[napi(readonly)]
    pub name: String,
}

/// Matches Hardhat's `MinimalEVMResult` interface.
#[napi(object)]
pub struct TracingMessageResult {
    /// The execution result
    #[napi(readonly)]
    pub exec_result: TracingExecResult,
}

/// Matches Hardhat's `MinimalExecResult` interface.
#[napi(object)]
pub struct TracingExecResult {
    /// Whether execution succeeded
    #[napi(readonly)]
    pub success: bool,
    /// Gas used during execution
    #[napi(readonly)]
    pub execution_gas_used: BigInt,
    /// Address of the created contract, if any
    #[napi(readonly)]
    pub contract_address: Option<Uint8Array>,
    /// The reason for the exit (success or halt)
    #[napi(readonly)]
    pub reason: Option<Either<SuccessReason, ExceptionalHalt>>,
    /// The output data
    #[napi(readonly)]
    pub output: Option<Uint8Array>,
}

pub(crate) fn u256_to_bigint(v: &edr_primitives::U256) -> BigInt {
    BigInt {
        sign_bit: false,
        words: v.into_limbs().to_vec(),
    }
}

#[napi]
#[derive(Clone)]
pub struct RawTrace {
    arena: CallTraceArena,
    verbose: bool,
}

impl RawTrace {
    /// Creates a new `RawTrace` from a `CallTraceArena` and verbose flag.
    pub fn new(arena: CallTraceArena, verbose: bool) -> Self {
        Self { arena, verbose }
    }
}

#[napi]
impl RawTrace {
    #[napi(getter)]
    pub fn trace(&self) -> Vec<Either3<TracingMessage, TracingStep, TracingMessageResult>> {
        let mut result = Vec::new();
        if !self.arena.nodes().is_empty() {
            convert_node(&self.arena, 0, self.verbose, &mut result);
        }
        result
    }
}

/// DFS traversal of the arena, emitting Before/Step/After messages in the flat
/// order that the old `TraceCollector` used to produce.
fn convert_node(
    arena: &CallTraceArena,
    node_idx: usize,
    verbose: bool,
    output: &mut Vec<Either3<TracingMessage, TracingStep, TracingMessageResult>>,
) {
    let node = &arena.nodes()[node_idx];
    let trace = &node.trace;

    // 1. Emit BeforeMessage (TracingMessage)
    let is_create = trace.kind.is_any_create();

    output.push(Either3::A(TracingMessage {
        caller: Uint8Array::with_data_copied(trace.caller.as_slice()),
        to: if is_create {
            None
        } else {
            Some(Uint8Array::with_data_copied(trace.address.as_slice()))
        },
        code_address: if is_create {
            None
        } else {
            Some(Uint8Array::with_data_copied(trace.address.as_slice()))
        },
        value: u256_to_bigint(&trace.value),
        data: Uint8Array::with_data_copied(&trace.data),
        gas_limit: BigInt::from(trace.gas_limit),
        is_static_call: trace.kind == CallKind::StaticCall,
    }));

    // 2. Walk ordering entries
    for ord in &node.ordering {
        match *ord {
            TraceMemberOrder::Step(i) => {
                let step = &trace.steps[i];
                let stack = if verbose {
                    // Full stack
                    step.stack
                        .as_ref()
                        .map(|s| s.iter().map(u256_to_bigint).collect())
                        .unwrap_or_default()
                } else {
                    // Top of stack only
                    step.stack
                        .as_ref()
                        .and_then(|s| s.last().map(|v| vec![u256_to_bigint(v)]))
                        .unwrap_or_default()
                };
                let memory = step
                    .memory
                    .as_ref()
                    .map(|m| Uint8Array::with_data_copied(m.as_bytes()));

                output.push(Either3::B(TracingStep {
                    pc: step.pc as u32,
                    depth: trace.depth as u32,
                    opcode: TracingOpcode {
                        name: OpCode::name_by_op(step.op.get()).to_string(),
                    },
                    stack,
                    memory,
                }));
            }
            TraceMemberOrder::Call(i) => {
                let child_idx = node.children[i];
                convert_node(arena, child_idx, verbose, output);
            }
            TraceMemberOrder::Log(_) => {
                // Logs are not part of the old trace format
            }
        }
    }

    // 3. Emit AfterMessage (TracingMessageResult)
    let reason = convert_status(trace.status);

    let contract_address = if is_create && trace.success {
        Some(Uint8Array::with_data_copied(trace.address.as_slice()))
    } else {
        None
    };

    output.push(Either3::C(TracingMessageResult {
        exec_result: TracingExecResult {
            // Use the trace.success field directly since it accounts for
            // edge cases that SuccessOrHalt may not (e.g. when status is None)
            success: trace.success,
            execution_gas_used: BigInt::from(trace.gas_used),
            contract_address,
            reason,
            output: Some(Uint8Array::with_data_copied(&trace.output)),
        },
    }));
}

/// Converts an `InstructionResult` status into an optional reason.
fn convert_status(
    status: Option<InstructionResult>,
) -> Option<Either<SuccessReason, ExceptionalHalt>> {
    let status = status?;

    let success_or_halt: SuccessOrHalt<EvmHaltReason> = status.into();
    match success_or_halt {
        SuccessOrHalt::Success(reason) => Some(Either::A(SuccessReason::from(reason))),
        SuccessOrHalt::Revert => None,
        SuccessOrHalt::Halt(reason) => Some(Either::B(ExceptionalHalt::from(reason))),
        SuccessOrHalt::FatalExternalError | SuccessOrHalt::Internal(_) => None,
    }
}
