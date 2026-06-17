//! Disk-based tests for [`collect_eip712_canonical_types`], exercising the
//! on-disk file reading and import resolution that the in-memory unit tests in
//! the crate cannot.

use std::{collections::HashMap, path::PathBuf};

use edr_solidity_collector_eip712::{
    collect_eip712_types_for_file, CollectError, CollectionLookupError, ImportResolver,
};
use semver::Version;

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

fn solc() -> Version {
    Version::new(0, 8, 24)
}

#[test]
fn resolves_relative_imports() {
    let collection = collect_eip712_types_for_file(
        &fixture("relative/Root.sol"),
        &solc(),
        &ImportResolver::default(),
    )
    .expect("collection should succeed");

    assert_eq!(
        collection.get("Mail").unwrap().canonical_definition(),
        "Mail(Person from,Person to,string contents)Person(address wallet,string name)"
    );
}

#[test]
fn resolves_mapped_imports() {
    let mut import_map = HashMap::new();
    import_map.insert(
        "@lib/Token.sol".to_string(),
        fixture("mapped/lib/Token.sol"),
    );

    let collection = collect_eip712_types_for_file(
        &fixture("mapped/Root.sol"),
        &solc(),
        &ImportResolver::new(import_map),
    )
    .expect("collection should succeed");

    assert_eq!(
        collection.get("Payment").unwrap().canonical_definition(),
        "Payment(Token token,uint256 amount)Token(address addr,uint8 decimals)"
    );
}

#[test]
fn unmapped_import_leaves_dependency_unresolved_but_unit_builds() {
    // No import mapping supplied: the import is unresolved (a diagnostic, not a
    // hard error). `Payment` depends on the missing `Token`, so it is not
    // usable, but collection itself still succeeds.
    let collection = collect_eip712_types_for_file(
        &fixture("mapped/Root.sol"),
        &solc(),
        &ImportResolver::default(),
    )
    .expect("collection should still succeed despite the unresolved import");

    assert!(matches!(
        collection.get("Token"),
        Err(CollectionLookupError::NotFound(_))
    ));
}

#[test]
fn missing_root_file_is_an_error() {
    let error = collect_eip712_types_for_file(
        &fixture("does/not/exist.sol"),
        &solc(),
        &ImportResolver::default(),
    )
    .unwrap_err();
    assert!(matches!(error, CollectError::RootFileNotFound { .. }));
}

#[test]
fn unsupported_solc_version_is_an_error() {
    let error = collect_eip712_types_for_file(
        &fixture("relative/Root.sol"),
        &Version::new(0, 7, 6),
        &ImportResolver::default(),
    )
    .unwrap_err();
    assert!(matches!(error, CollectError::InvalidSolcVersion { .. }));
}
