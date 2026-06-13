//! Collects EIP-712 canonical type definitions from Solidity sources.
//!
//! This crate is a Rust port of Hardhat's TypeScript `collectEip712CanonicalTypes`.
//! Where the TypeScript walks solc JSON ASTs, this crate parses `.sol` files
//! directly with [Slang v2](https://github.com/NomicFoundation/slang) and walks
//! the resolved AST.
//!
//! It also hosts [`Eip712TypeDef`] — the canonical-form EIP-712 type definition
//! used by the EDR Solidity-test cheatcodes (`eip712HashType`,
//! `eip712HashStruct`) — so that producing and consuming canonical types share
//! a single type without conversions.

mod collector;
mod provider;
mod resolver;
mod types;

pub use crate::{
    collector::{
        collect_eip712_canonical_types, collect_from_compilation_unit, CollectError,
        Eip712Collection, LookupError,
    },
    provider::{CachedEip712TypeProvider, Eip712TypeProvider, SharedEip712TypeProvider},
    types::{Eip712Error, Eip712TypeDef},
};
