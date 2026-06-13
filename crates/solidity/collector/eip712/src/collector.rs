//! Collects EIP-712 canonical type definitions from Solidity sources by
//! parsing them with Slang v2 and walking the resolved AST.
//!
//! This is a Rust port of Hardhat's TypeScript `collectEip712CanonicalTypes`,
//! which walks solc JSON ASTs; here we parse `.sol` files directly with Slang.
//! The canonicalization semantics (member-type encoding, struct dependency
//! ordering, encodability propagation, deduplication) mirror that
//! implementation and `forge bind-json`.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use semver::Version;
use slang_solidity_v2::{
    ast::{Definition, Type},
    compilation::{CompilationBuilder, CompilationUnit},
    utils::LanguageVersion,
};

use crate::{resolver::DiskResolver, types::Eip712TypeDef};

/// A set of EIP-712 canonical type definitions collected from a compilation
/// unit, keyed by primary type name.
///
/// Names that were seen but cannot be used (a same-name conflict between two
/// files, a non-EIP-712-encodable member, or a transitively non-encodable
/// dependency) are recorded separately so a lookup can explain *why* a type is
/// unavailable rather than reporting a bare "not found".
#[derive(Clone, Debug, Default)]
pub struct Eip712Collection {
    types: HashMap<String, Eip712TypeDef>,
    rejected: HashMap<String, String>,
}

/// Why a [`Eip712Collection::get`] lookup did not return a type.
#[derive(Clone, Debug, thiserror::Error)]
pub enum LookupError {
    /// No struct with this name exists in the compilation unit.
    #[error("EIP-712 type '{0}' not found in the test contract's sources")]
    NotFound(String),

    /// A struct with this name exists but cannot be used as an EIP-712 type.
    #[error("EIP-712 type '{name}' cannot be used: {reason}")]
    Rejected {
        /// The requested type name.
        name: String,
        /// Why the type was rejected.
        reason: String,
    },
}

impl Eip712Collection {
    /// Looks up a canonical type definition by primary type name.
    pub fn get(&self, name: &str) -> Result<&Eip712TypeDef, LookupError> {
        if let Some(def) = self.types.get(name) {
            Ok(def)
        } else if let Some(reason) = self.rejected.get(name) {
            Err(LookupError::Rejected {
                name: name.to_string(),
                reason: reason.clone(),
            })
        } else {
            Err(LookupError::NotFound(name.to_string()))
        }
    }

    /// Number of usable (encodable) type definitions.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether there are no usable type definitions.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

/// Errors that prevent collection from running at all (as opposed to per-type
/// rejections, which are surfaced lazily via [`Eip712Collection::get`]).
#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    /// The solc version is outside the range Slang v2 can parse (`< 0.8.0`).
    #[error(
        "unsupported solc version {version} for EIP-712 source parsing \
         (Slang v2 supports 0.8.0 and later)"
    )]
    UnsupportedSolcVersion {
        /// The offending version.
        version: Version,
    },

    /// The root source file could not be read.
    #[error("could not read EIP-712 root source {path}: {reason}")]
    RootFileNotFound {
        /// The root source path.
        path: String,
        /// Why it could not be read.
        reason: String,
    },
}

/// Collects EIP-712 canonical types reachable from `root_source`.
///
/// `import_map` maps non-relative import paths (as written in `import`
/// statements) to absolute disk paths; relative imports are resolved against
/// the importing file. Parse errors and unresolved imports degrade gracefully
/// — structs that still resolve are collected — but a missing root file is a
/// hard error.
pub fn collect_eip712_canonical_types(
    root_source: &Path,
    solc_version: &Version,
    import_map: &HashMap<String, PathBuf>,
) -> Result<Eip712Collection, CollectError> {
    let language_version = to_language_version(solc_version)?;

    // Pre-check the root: a build over a missing root only yields a diagnostic
    // and an empty unit, which we would otherwise mistake for "no types".
    if let Err(error) = std::fs::metadata(root_source) {
        return Err(CollectError::RootFileNotFound {
            path: root_source.display().to_string(),
            reason: error.to_string(),
        });
    }

    let mut builder =
        CompilationBuilder::create(language_version, DiskResolver::new(import_map.clone()));
    builder.add_file(root_source.to_string_lossy().into_owned());
    let unit = builder.build();

    Ok(collect_from_compilation_unit(&unit))
}

/// Maps a solc [`Version`] to a Slang [`LanguageVersion`], stripping
/// pre-release/build metadata and clamping versions newer than Slang supports
/// down to its latest grammar.
fn to_language_version(solc_version: &Version) -> Result<LanguageVersion, CollectError> {
    let cleaned = Version::new(solc_version.major, solc_version.minor, solc_version.patch);
    if let Ok(version) = LanguageVersion::try_from(cleaned.clone()) {
        return Ok(version);
    }

    // try_from only fails for versions outside [0.8.0, LATEST]. Clamp anything
    // newer to LATEST; reject anything older (no <0.8 grammar in Slang v2).
    let latest: Version = LanguageVersion::LATEST.into();
    if cleaned > latest {
        Ok(LanguageVersion::LATEST)
    } else {
        Err(CollectError::UnsupportedSolcVersion {
            version: solc_version.clone(),
        })
    }
}

/// Core collection logic, decoupled from disk so it can be unit-tested against
/// an in-memory compilation unit.
pub fn collect_from_compilation_unit(unit: &CompilationUnit) -> Eip712Collection {
    let collected = collect_structs(unit);
    let all_struct_names: HashSet<String> = collected.iter().map(|s| s.name.clone()).collect();

    let (by_name, mut rejected) = index_by_name(collected);
    mark_non_encodable(&by_name, &all_struct_names, &mut rejected);
    emit(&by_name, &all_struct_names, rejected)
}

/// A struct definition collected from the AST, with each member's type already
/// encoded to its EIP-712 form (`None` if the member is not encodable).
struct CollectedStruct {
    name: String,
    file_id: String,
    members: Vec<CollectedMember>,
}

struct CollectedMember {
    name: String,
    /// EIP-712 encoded member type, or `None` if not encodable (mapping,
    /// function, fixed-point, unresolved, …).
    encoded_type: Option<String>,
}

/// Walks every struct definition in the unit and encodes its members.
fn collect_structs(unit: &CompilationUnit) -> Vec<CollectedStruct> {
    let mut collected = Vec::new();
    for definition in unit.all_definitions() {
        let Definition::Struct(struct_def) = definition else {
            continue;
        };
        let members = struct_def
            .members()
            .iter()
            .map(|member| CollectedMember {
                name: member.name().unparse().to_string(),
                encoded_type: member.get_type().and_then(|ty| encode_member_type(&ty)),
            })
            .collect();
        collected.push(CollectedStruct {
            name: struct_def.name().unparse().to_string(),
            file_id: struct_def.get_file_id().to_string(),
            members,
        });
    }
    collected
}

/// Encodes a resolved member type to its EIP-712 form, following the same
/// conventions as `forge bind-json`: enums become `uint8`, contracts/addresses
/// become `address`, user-defined value types resolve to their underlying
/// elementary type, structs become their bare name (a dependency), and
/// non-encodable types (mappings, functions, fixed-point) yield `None`.
fn encode_member_type(ty: &Type) -> Option<String> {
    match ty {
        Type::Address(_) | Type::Contract(_) | Type::Interface(_) | Type::Library(_) => {
            Some("address".to_string())
        }
        Type::Boolean(_) => Some("bool".to_string()),
        Type::Integer(integer) => {
            let prefix = if integer.signed() { "" } else { "u" };
            let bits = integer.bits();
            Some(format!("{prefix}int{bits}"))
        }
        Type::ByteArray(byte_array) => {
            let width = byte_array.width();
            Some(format!("bytes{width}"))
        }
        Type::Bytes(_) => Some("bytes".to_string()),
        Type::String(_) => Some("string".to_string()),
        Type::Enum(_) => Some("uint8".to_string()),
        Type::Struct(struct_type) => match struct_type.definition() {
            Definition::Struct(struct_def) => Some(struct_def.name().unparse().to_string()),
            _ => None,
        },
        Type::UserDefinedValue(udvt) => udvt.target_type().as_ref().and_then(encode_member_type),
        Type::Array(array) => {
            let base = encode_member_type(&array.element_type())?;
            Some(format!("{base}[]"))
        }
        Type::FixedSizeArray(array) => {
            let base = encode_member_type(&array.element_type())?;
            let size = array.size();
            Some(format!("{base}[{size}]"))
        }
        Type::Mapping(_)
        | Type::Function(_)
        | Type::FixedPointNumber(_)
        | Type::Tuple(_)
        | Type::Literal(_)
        | Type::Void(_) => None,
    }
}

/// Indexes collected structs by name. Identical definitions (same fingerprint)
/// dedupe silently; conflicting same-name definitions are removed and recorded
/// as rejected.
fn index_by_name(
    collected: Vec<CollectedStruct>,
) -> (HashMap<String, CollectedStruct>, HashMap<String, String>) {
    let mut by_name: HashMap<String, CollectedStruct> = HashMap::new();
    let mut fingerprints: HashMap<String, String> = HashMap::new();
    let mut rejected: HashMap<String, String> = HashMap::new();

    for struct_def in collected {
        if rejected.contains_key(&struct_def.name) {
            continue;
        }

        let fingerprint = fingerprint(&struct_def);
        match fingerprints.get(&struct_def.name) {
            None => {
                fingerprints.insert(struct_def.name.clone(), fingerprint);
                by_name.insert(struct_def.name.clone(), struct_def);
            }
            // Identical re-definition (e.g. the same struct seen via two
            // imports): keep one, no error.
            Some(existing) if *existing == fingerprint => {}
            // Same name, different body: ambiguous. Drop both and reject.
            Some(_) => {
                let first_file = by_name
                    .remove(&struct_def.name)
                    .map(|s| s.file_id)
                    .unwrap_or_default();
                fingerprints.remove(&struct_def.name);
                rejected.insert(
                    struct_def.name.clone(),
                    format!(
                        "conflicting definitions of struct '{}' in {} and {}",
                        struct_def.name, first_file, struct_def.file_id
                    ),
                );
            }
        }
    }

    (by_name, rejected)
}

/// A deterministic fingerprint of a struct's name and members (including
/// non-encodable members as `<unsupported>`), used to tell identical
/// re-definitions apart from genuine conflicts.
fn fingerprint(struct_def: &CollectedStruct) -> String {
    let members: Vec<String> = struct_def
        .members
        .iter()
        .map(|member| {
            let ty = member.encoded_type.as_deref().unwrap_or("<unsupported>");
            let name = &member.name;
            format!("{ty} {name}")
        })
        .collect();
    let name = &struct_def.name;
    let body = members.join(",");
    format!("{name}({body})")
}

/// Records, in `rejected`, every struct that cannot be EIP-712 encoded: those
/// with a non-encodable member, then (by fixed point) those that depend on a
/// rejected or non-encodable struct.
fn mark_non_encodable(
    by_name: &HashMap<String, CollectedStruct>,
    all_struct_names: &HashSet<String>,
    rejected: &mut HashMap<String, String>,
) {
    // Seed: structs with a directly non-encodable member.
    for (name, struct_def) in by_name {
        if rejected.contains_key(name) {
            continue;
        }
        if let Some(bad) = struct_def.members.iter().find(|m| m.encoded_type.is_none()) {
            let member = &bad.name;
            rejected.insert(
                name.clone(),
                format!("member '{member}' has a type that is not EIP-712 encodable"),
            );
        }
    }

    // Propagate: a struct depending on a rejected/unusable struct is itself
    // unusable. Iterate to a fixed point.
    loop {
        let mut newly_rejected: Vec<(String, String)> = Vec::new();
        for (name, struct_def) in by_name {
            if rejected.contains_key(name) {
                continue;
            }
            for dependency in direct_struct_deps(struct_def, all_struct_names) {
                let reason = if rejected.contains_key(&dependency) {
                    Some(format!("depends on non-encodable struct '{dependency}'"))
                } else if !by_name.contains_key(&dependency) {
                    // A known struct name that isn't usable (it was dropped as
                    // a conflict but the conflict reason lives under its name).
                    Some(format!("depends on ambiguous struct '{dependency}'"))
                } else {
                    None
                };
                if let Some(reason) = reason {
                    newly_rejected.push((name.clone(), reason));
                    break;
                }
            }
        }

        if newly_rejected.is_empty() {
            break;
        }
        rejected.extend(newly_rejected);
    }
}

/// The names of structs directly referenced by a struct's members (array
/// suffixes stripped, self-references excluded).
fn direct_struct_deps(
    struct_def: &CollectedStruct,
    all_struct_names: &HashSet<String>,
) -> Vec<String> {
    let mut deps = Vec::new();
    for member in &struct_def.members {
        let Some(encoded) = &member.encoded_type else {
            continue;
        };
        let base = base_type_name(encoded);
        if base != struct_def.name && all_struct_names.contains(base) {
            deps.push(base.to_string());
        }
    }
    deps
}

/// Strips array suffixes from an encoded type to get its base name
/// (`Person[3][2]` -> `Person`).
fn base_type_name(encoded_type: &str) -> &str {
    match encoded_type.split_once('[') {
        Some((base, _)) => base,
        None => encoded_type,
    }
}

/// Emits canonical strings for every encodable struct and parses them into
/// [`Eip712TypeDef`]s (which re-validates the canonical-form invariant).
fn emit(
    by_name: &HashMap<String, CollectedStruct>,
    all_struct_names: &HashSet<String>,
    mut rejected: HashMap<String, String>,
) -> Eip712Collection {
    let mut types = HashMap::new();
    let mut parse_failures: Vec<(String, String)> = Vec::new();

    for (name, struct_def) in by_name {
        if rejected.contains_key(name) {
            continue;
        }

        let mut dependency_heads: Vec<String> =
            transitive_struct_deps(struct_def, by_name, all_struct_names)
                .into_iter()
                .filter_map(|dependency| by_name.get(&dependency).map(struct_head))
                .collect();
        dependency_heads.sort();

        let mut canonical = struct_head(struct_def);
        canonical.push_str(&dependency_heads.concat());

        match Eip712TypeDef::parse(&canonical) {
            Ok(type_def) => {
                types.insert(name.clone(), type_def);
            }
            Err(error) => {
                parse_failures.push((name.clone(), format!("failed to canonicalize: {error}")));
            }
        }
    }

    rejected.extend(parse_failures);
    Eip712Collection { types, rejected }
}

/// The `Name(type member,…)` head of a single struct. Only called on encodable
/// structs, whose members all have an encoded type.
fn struct_head(struct_def: &CollectedStruct) -> String {
    let members: Vec<String> = struct_def
        .members
        .iter()
        .map(|member| {
            let ty = member.encoded_type.as_deref().unwrap_or_default();
            let name = &member.name;
            format!("{ty} {name}")
        })
        .collect();
    let name = &struct_def.name;
    let body = members.join(",");
    format!("{name}({body})")
}

/// All structs transitively referenced by `root` (excluding `root` itself).
fn transitive_struct_deps(
    root: &CollectedStruct,
    by_name: &HashMap<String, CollectedStruct>,
    all_struct_names: &HashSet<String>,
) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack = direct_struct_deps(root, all_struct_names);
    while let Some(next) = stack.pop() {
        if next == root.name || visited.contains(&next) {
            continue;
        }
        visited.insert(next.clone());
        if let Some(dependency) = by_name.get(&next) {
            stack.extend(direct_struct_deps(dependency, all_struct_names));
        }
    }
    visited.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use slang_solidity_v2::compilation::CompilationBuilderConfig;

    use super::*;

    /// A [`CompilationBuilderConfig`] that serves sources from memory. File ids
    /// and import paths are one and the same, so imports in test sources are
    /// written as the bare file id (e.g. `import "dep.sol";`).
    struct InMemorySources {
        sources: HashMap<String, String>,
    }

    impl CompilationBuilderConfig for InMemorySources {
        fn read_file(&mut self, file_id: &str) -> Result<String, String> {
            self.sources
                .get(file_id)
                .cloned()
                .ok_or_else(|| format!("no such file: {file_id}"))
        }

        fn resolve_import(&mut self, _source: &str, import_path: &str) -> Result<String, String> {
            if self.sources.contains_key(import_path) {
                Ok(import_path.to_string())
            } else {
                Err(format!("unresolved import: {import_path}"))
            }
        }
    }

    /// Builds a compilation unit from in-memory sources (the first entry is the
    /// root) and collects EIP-712 types from it.
    fn collect(sources: &[(&str, &str)]) -> Eip712Collection {
        let (root, _) = sources.first().expect("at least one source");
        let map = sources
            .iter()
            .map(|(id, src)| ((*id).to_string(), (*src).to_string()))
            .collect();
        let mut builder = CompilationBuilder::create(
            LanguageVersion::LATEST,
            InMemorySources { sources: map },
        );
        builder.add_file((*root).to_string());
        let unit = builder.build();
        collect_from_compilation_unit(&unit)
    }

    /// Convenience: collect from a single root source.
    fn collect_one(source: &str) -> Eip712Collection {
        collect(&[("root.sol", source)])
    }

    fn canonical<'a>(collection: &'a Eip712Collection, name: &str) -> &'a str {
        collection
            .get(name)
            .unwrap_or_else(|error| panic!("expected '{name}': {error}"))
            .canonical_definition()
    }

    #[test]
    fn eip712_spec_mail_person() {
        // The canonical example from https://eips.ethereum.org/EIPS/eip-712.
        let collection = collect_one(
            "struct Person { address wallet; string name; }
             struct Mail { Person from; Person to; string contents; }",
        );
        assert_eq!(
            canonical(&collection, "Mail"),
            "Mail(Person from,Person to,string contents)Person(address wallet,string name)"
        );
        assert_eq!(
            canonical(&collection, "Person"),
            "Person(address wallet,string name)"
        );
    }

    #[test]
    fn dependencies_sorted_alphabetically() {
        let collection = collect_one(
            "struct Person { address wallet; string name; }
             struct Asset { address token; uint256 amount; }
             struct Transaction { Person from; Asset payload; }",
        );
        // Asset sorts before Person regardless of member order.
        assert_eq!(
            canonical(&collection, "Transaction"),
            "Transaction(Person from,Asset payload)\
             Asset(address token,uint256 amount)\
             Person(address wallet,string name)"
        );
    }

    #[test]
    fn transitive_dependencies_included_once() {
        let collection = collect_one(
            "struct C { uint256 v; }
             struct B { C c; }
             struct A { B b; C c; }",
        );
        assert_eq!(
            canonical(&collection, "A"),
            "A(B b,C c)B(C c)C(uint256 v)"
        );
    }

    #[test]
    fn self_recursive_struct_is_rejected() {
        // EIP-712's type system is acyclic: a struct that references itself
        // has no finite type hash, so canonicalization rejects it.
        let collection = collect_one("struct Node { uint256 value; Node[] children; }");
        assert!(
            matches!(collection.get("Node"), Err(LookupError::Rejected { .. })),
            "recursive struct should be rejected"
        );
    }

    #[test]
    fn mutually_recursive_structs_are_rejected() {
        // Same as self-recursion: a cycle (A -> B -> A) has no finite hash.
        let collection = collect_one(
            "struct A { B b; }
             struct B { A a; }",
        );
        assert!(matches!(
            collection.get("A"),
            Err(LookupError::Rejected { .. })
        ));
        assert!(matches!(
            collection.get("B"),
            Err(LookupError::Rejected { .. })
        ));
    }

    #[test]
    fn enum_member_is_uint8() {
        let collection = collect_one(
            "enum Color { Red, Green, Blue }
             struct S { Color color; }",
        );
        assert_eq!(canonical(&collection, "S"), "S(uint8 color)");
    }

    #[test]
    fn contract_interface_library_members_are_address() {
        let collection = collect_one(
            "contract C {}
             interface I {}
             library L {}
             struct S { C c; I i; }",
        );
        assert_eq!(canonical(&collection, "S"), "S(address c,address i)");
    }

    #[test]
    fn user_defined_value_type_resolves_to_underlying() {
        let collection = collect_one(
            "type USD is uint256;
             struct S { USD amount; }",
        );
        assert_eq!(canonical(&collection, "S"), "S(uint256 amount)");
    }

    #[test]
    fn user_defined_value_type_resolves_across_files() {
        let collection = collect(&[
            (
                "root.sol",
                "import \"udvt.sol\";
                 struct S { USD amount; }",
            ),
            ("udvt.sol", "type USD is uint128;"),
        ]);
        assert_eq!(canonical(&collection, "S"), "S(uint128 amount)");
    }

    #[test]
    fn address_payable_is_address() {
        let collection = collect_one("struct S { address payable recipient; }");
        assert_eq!(canonical(&collection, "S"), "S(address recipient)");
    }

    #[test]
    fn integer_aliases_are_normalized() {
        let collection = collect_one("struct S { uint a; int b; }");
        assert_eq!(canonical(&collection, "S"), "S(uint256 a,int256 b)");
    }

    #[test]
    fn byte_and_string_types() {
        let collection = collect_one("struct S { bytes data; string text; bytes17 fixed_bytes; }");
        assert_eq!(
            canonical(&collection, "S"),
            "S(bytes data,string text,bytes17 fixed_bytes)"
        );
    }

    #[test]
    fn arrays_dynamic_fixed_and_nested() {
        let collection =
            collect_one("struct S { uint256[] dynamic; uint256[3] fixed_size; uint256[3][2] nested; }");
        assert_eq!(
            canonical(&collection, "S"),
            "S(uint256[] dynamic,uint256[3] fixed_size,uint256[3][2] nested)"
        );
    }

    #[test]
    fn array_of_structs() {
        let collection = collect_one(
            "struct Person { address wallet; string name; }
             struct Group { Person[] members; }",
        );
        assert_eq!(
            canonical(&collection, "Group"),
            "Group(Person[] members)Person(address wallet,string name)"
        );
    }

    #[test]
    fn mapping_member_makes_struct_non_encodable() {
        let collection = collect_one("struct S { mapping(uint256 => uint256) balances; }");
        assert!(matches!(
            collection.get("S"),
            Err(LookupError::Rejected { .. })
        ));
    }

    #[test]
    fn non_encodability_propagates_to_dependents() {
        let collection = collect_one(
            "struct Inner { mapping(uint256 => uint256) m; }
             struct Outer { Inner inner; }",
        );
        assert!(matches!(
            collection.get("Inner"),
            Err(LookupError::Rejected { .. })
        ));
        let outer = collection.get("Outer").unwrap_err();
        assert!(
            matches!(&outer, LookupError::Rejected { reason, .. } if reason.contains("Inner")),
            "unexpected: {outer}"
        );
    }

    #[test]
    fn function_typed_member_is_non_encodable() {
        let collection = collect_one("struct S { function() external fn; }");
        assert!(matches!(
            collection.get("S"),
            Err(LookupError::Rejected { .. })
        ));
    }

    #[test]
    fn file_level_and_contract_nested_structs_both_collected() {
        let collection = collect_one(
            "struct TopLevel { uint256 a; }
             contract C { struct Nested { uint256 b; } }",
        );
        assert_eq!(canonical(&collection, "TopLevel"), "TopLevel(uint256 a)");
        assert_eq!(canonical(&collection, "Nested"), "Nested(uint256 b)");
    }

    #[test]
    fn identical_duplicate_definitions_dedupe() {
        let collection = collect(&[
            (
                "root.sol",
                "import \"other.sol\";
                 struct S { uint256 a; }",
            ),
            ("other.sol", "struct S { uint256 a; }"),
        ]);
        assert_eq!(canonical(&collection, "S"), "S(uint256 a)");
    }

    #[test]
    fn conflicting_definitions_are_rejected_others_unaffected() {
        let collection = collect_one(
            "struct S { uint256 a; }
             contract C { struct S { uint256 b; } }
             struct Ok { uint256 c; }",
        );
        assert!(matches!(
            collection.get("S"),
            Err(LookupError::Rejected { .. })
        ));
        // An unrelated struct is still usable.
        assert_eq!(canonical(&collection, "Ok"), "Ok(uint256 c)");
    }

    #[test]
    fn dependent_of_conflicting_struct_is_rejected() {
        let collection = collect_one(
            "struct S { uint256 a; }
             contract C { struct S { uint256 b; } }
             struct Uses { S s; }",
        );
        let uses = collection.get("Uses").unwrap_err();
        assert!(
            matches!(&uses, LookupError::Rejected { reason, .. } if reason.contains('S')),
            "unexpected: {uses}"
        );
    }

    #[test]
    fn import_aliasing_uses_definition_name() {
        let collection = collect(&[
            (
                "root.sol",
                "import { Person as Account } from \"person.sol\";
                 struct Wallet { Account owner; }",
            ),
            (
                "person.sol",
                "struct Person { address addr; string handle; }",
            ),
        ]);
        // The dependency is encoded under its definition name, not the alias.
        assert_eq!(
            canonical(&collection, "Wallet"),
            "Wallet(Person owner)Person(address addr,string handle)"
        );
    }

    #[test]
    fn unknown_type_is_not_found() {
        let collection = collect_one("struct S { uint256 a; }");
        assert!(matches!(
            collection.get("DoesNotExist"),
            Err(LookupError::NotFound(_))
        ));
    }

    mod version_mapping {
        use super::*;

        #[test]
        fn exact_supported_version() {
            assert_eq!(
                to_language_version(&Version::new(0, 8, 24)).unwrap(),
                LanguageVersion::V0_8_24
            );
        }

        #[test]
        fn strips_build_and_prerelease_metadata() {
            let version = Version::parse("0.8.24+commit.abcdef").unwrap();
            assert_eq!(
                to_language_version(&version).unwrap(),
                LanguageVersion::V0_8_24
            );
        }

        #[test]
        fn clamps_newer_versions_to_latest() {
            assert_eq!(
                to_language_version(&Version::new(0, 9, 0)).unwrap(),
                LanguageVersion::LATEST
            );
        }

        #[test]
        fn rejects_versions_older_than_0_8_0() {
            assert!(matches!(
                to_language_version(&Version::new(0, 7, 6)),
                Err(CollectError::UnsupportedSolcVersion { .. })
            ));
        }
    }
}
