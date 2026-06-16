//! Defines EIP-712 types and a means of collecting EIP-712 canonical type
//! definitions from Solidity sources.

mod collector;
mod provider;
mod resolver;
mod types;

pub use crate::{
    collector::{
        collect_eip712_types_for_file, collect_eip712_types_from_compilation_unit, CollectError,
        Eip712Collection, LookupError,
    },
    provider::{AsyncEip712Provider, CachedEip712Provider, Eip712Root},
    resolver::ImportResolver,
    types::{Eip712Error, Eip712TypeDef},
};
