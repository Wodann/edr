//! Two-phase EIP-712 canonical type resolution.
//!
//! Phase 1 (collection): given the set of source files to cover, parse each and
//! collect its EIP-712 canonical types. [`Eip712Provider`] does this eagerly and
//! synchronously; [`AsyncEip712Provider`] wraps it, running the per-root parses
//! in parallel on a background thread.
//!
//! Phase 2 (query): look a type up by name within the scope of one source file.
//! For [`AsyncEip712Provider`], a query blocks until background collection has
//! finished.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
};

use rayon::prelude::*;
use semver::Version;

use crate::{collect_eip712_types_for_file, resolver::ImportResolver, Eip712TypeDef};

/// A Solidity source file to collect EIP-712 canonical types from.
#[derive(Clone, Debug)]
pub struct Eip712Root {
    /// The identity this root is queried by — the compiled artifact's solc
    /// source name (e.g. the running test contract's `source`). Collections are
    /// keyed by this, not by the on-disk path, because that is what a query has
    /// in hand.
    pub source: PathBuf,
    /// Absolute path to the file on disk, used to read and parse it.
    pub path: PathBuf,
    /// The solc version the file was compiled with.
    pub version: Version,
}

/// EIP-712 canonical types collected per source file, queryable by type name.
///
/// Built by [`Eip712Provider::collect`] (eager, synchronous) or by
/// [`AsyncEip712Provider`] (background, parallel). The per-root collection
/// result is retained even on failure so a query can report *why* a scope has
/// no types.
#[derive(Clone, Debug, Default)]
pub struct Eip712Provider {
    by_source: HashMap<PathBuf, Result<crate::Eip712Collection, String>>,
}

impl Eip712Provider {
    /// Collects EIP-712 canonical types for every root, sequentially. Parses
    /// whatever it is given — it makes no assumptions about which roots are
    /// relevant.
    pub fn collect(roots: &[Eip712Root], import_resolver: &ImportResolver) -> Self {
        let by_source = roots
            .iter()
            .map(|root| (root.source.clone(), collect_root(root, import_resolver)))
            .collect();
        Self { by_source }
    }

    /// An empty provider with no collected types.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Looks up a canonical type by name within the scope of `source`.
    pub fn get(&self, source: &Path, type_name: &str) -> Result<Eip712TypeDef, String> {
        match self.by_source.get(source) {
            Some(Ok(collection)) => collection
                .get(type_name)
                .cloned()
                .map_err(|error| error.to_string()),
            Some(Err(reason)) => Err(reason.clone()),
            None => Err(format!(
                "no EIP-712 types were collected for source '{}'",
                source.display()
            )),
        }
    }
}

/// Collects one root, stringifying any collection error so it can be stored and
/// replayed at query time.
fn collect_root(root: &Eip712Root, import_resolver: &ImportResolver) -> Result<crate::Eip712Collection, String> {
    collect_eip712_types_for_file(&root.path, &root.version, import_resolver)
        .map_err(|error| error.to_string())
}

/// Shared latch holding the eventual [`Eip712Provider`] once background
/// collection completes.
#[derive(Debug)]
struct Collection {
    provider: Mutex<Option<Eip712Provider>>,
    ready: Condvar,
}

/// A cloneable, `Send + Sync` handle that collects EIP-712 types in the
/// background, parallelizing the per-root parses (via [`rayon`]). Queries block
/// until collection has finished.
#[derive(Clone, Debug)]
pub struct AsyncEip712Provider {
    collection: Arc<Collection>,
}

impl AsyncEip712Provider {
    /// Spawns a background thread that collects every root in parallel and then
    /// makes the resulting [`Eip712Provider`] available to queries.
    pub fn new(roots: Vec<Eip712Root>, import_resolver: ImportResolver) -> Self {
        let collection = Arc::new(Collection {
            provider: Mutex::new(None),
            ready: Condvar::new(),
        });

        let background = Arc::clone(&collection);
        thread::Builder::new()
            .name("eip712-collector".to_string())
            .spawn(move || {
                let by_source = roots
                    .par_iter()
                    .map(|root| (root.source.clone(), collect_root(root, &import_resolver)))
                    .collect();
                let provider = Eip712Provider { by_source };

                *background
                    .provider
                    .lock()
                    .expect("EIP-712 collection mutex poisoned") = Some(provider);
                background.ready.notify_all();
            })
            .expect("EIP-712 collector thread should spawn");

        Self { collection }
    }

    /// Looks up a canonical type by name within the scope of `source`, blocking
    /// until background collection has finished.
    pub fn get(&self, source: &Path, type_name: &str) -> Result<Eip712TypeDef, String> {
        let mut provider = self
            .collection
            .provider
            .lock()
            .expect("EIP-712 collection mutex poisoned");
        while provider.is_none() {
            provider = self
                .collection
                .ready
                .wait(provider)
                .expect("EIP-712 collection mutex poisoned");
        }
        provider
            .as_ref()
            .expect("provider is populated after the wait loop")
            .get(source, type_name)
    }
}

// NOTE (Default): `AsyncEip712Provider` intentionally has no `Default` impl — an
// EIP-712 provider is meaningless without the roots it covers. `CheatsConfigOptions`
// currently derives `Default` and is `..default()`-ed in two non-production
// spots (a `TestRunnerConfig` `Default` impl and an `edr_solidity_tests` test
// helper). Deciding how to satisfy that — give this an empty `Default`, model
// the field as `Option`/an enum, or drop `Default` and construct explicitly —
// is left open here on purpose.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    /// A root whose query identity (`source`) is the relative fixture path and
    /// whose on-disk `path` is the absolute location.
    fn root(relative: &str) -> Eip712Root {
        Eip712Root {
            source: PathBuf::from(relative),
            path: fixtures_root().join(relative),
            version: Version::new(0, 8, 24),
        }
    }

    fn mail_canonical() -> &'static str {
        "Mail(Person from,Person to,string contents)Person(address wallet,string name)"
    }

    #[test]
    fn sync_provider_collects_and_queries_by_scope() {
        let provider = Eip712Provider::collect(
            &[root("relative/Root.sol")],
            &ImportResolver::default(),
        );

        assert_eq!(
            provider
                .get(Path::new("relative/Root.sol"), "Mail")
                .unwrap()
                .canonical_definition(),
            mail_canonical()
        );
        assert!(provider
            .get(Path::new("relative/Root.sol"), "DoesNotExist")
            .is_err());
        // A scope that was never collected reports as much.
        assert!(provider.get(Path::new("never/collected.sol"), "Mail").is_err());
    }

    #[test]
    fn sync_provider_resolves_mapped_imports() {
        let mut import_map = HashMap::new();
        import_map.insert(
            "@lib/Token.sol".to_string(),
            fixtures_root().join("mapped/lib/Token.sol"),
        );
        let provider =
            Eip712Provider::collect(&[root("mapped/Root.sol")], &ImportResolver::new(import_map));

        assert_eq!(
            provider
                .get(Path::new("mapped/Root.sol"), "Payment")
                .unwrap()
                .canonical_definition(),
            "Payment(Token token,uint256 amount)Token(address addr,uint8 decimals)"
        );
    }

    #[test]
    fn async_provider_collects_all_roots_then_serves() {
        let provider = AsyncEip712Provider::new(
            vec![root("relative/Root.sol"), root("relative/Dep.sol")],
            ImportResolver::default(),
        );

        assert_eq!(
            provider
                .get(Path::new("relative/Root.sol"), "Mail")
                .unwrap()
                .canonical_definition(),
            mail_canonical()
        );
        assert_eq!(
            provider
                .get(Path::new("relative/Dep.sol"), "Person")
                .unwrap()
                .canonical_definition(),
            "Person(address wallet,string name)"
        );
    }

    #[test]
    fn async_provider_blocks_concurrent_queries_until_ready() {
        let provider = Arc::new(AsyncEip712Provider::new(
            vec![root("relative/Root.sol")],
            ImportResolver::default(),
        ));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let provider = Arc::clone(&provider);
                thread::spawn(move || {
                    provider
                        .get(Path::new("relative/Root.sol"), "Person")
                        .unwrap()
                        .canonical_definition()
                        .to_string()
                })
            })
            .collect();

        for handle in handles {
            assert_eq!(handle.join().unwrap(), "Person(address wallet,string name)");
        }
    }
}
