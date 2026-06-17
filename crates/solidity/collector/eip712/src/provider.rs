//! Two-phase EIP-712 canonical type resolution.
//!
//! Phase 1 (collection): given the set of source files to cover, parse each and
//! collect its EIP-712 canonical types. [`Eip712Provider`] does this eagerly
//! and synchronously; [`AsyncEip712Provider`] wraps it, running the per-root
//! parses in parallel on a background thread.
//!
//! Phase 2 (query): look a type up by name within the scope of one source file.
//! For [`AsyncEip712Provider`], a query blocks until background collection has
//! finished.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{mpsc, Arc,},
    thread,
};

use rayon::prelude::*;
use semver::Version;

use crate::{
    CollectError, Eip712Type, collector::{Eip712CollectionLookupError, Eip712TypeCollection, Eip712TypeRejected, collect_eip712_types_for_file}, resolver::ImportResolver 
};

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

/// Why a [`Eip712Collection::get`] lookup did not return a type.
#[derive(Debug, thiserror::Error)]
pub enum Eip712LookupError {
    /// No struct with this name exists in the compilation unit.
    #[error("EIP-712 type '{type_name}' was not found in source '{source_path}'.")]
    NotFound {
        source_path: String,
        type_name: String,
    },
    /// A struct with this name exists but cannot be used as an EIP-712 type.
    #[error(transparent)]
    Rejected(#[from] Eip712TypeRejected),
    /// The source file was not found in the provider.
    #[error("The source file '{source_path}' was not found.")]
    UnknownSource {
        /// The source file path that was not found in the provider.
        source_path: String,
    },
}

/// EIP-712 canonical types collected per source file, queryable by type name.
///
/// Built by [`Eip712Provider::collect`] (eager, synchronous) or by
/// [`AsyncEip712Provider`] (background, parallel). The per-root collection
/// result is retained even on failure so a query can report *why* a scope has
/// no types.
#[derive(Debug, Default)]
pub struct CachedEip712Provider {
    by_source: HashMap<PathBuf, Eip712TypeCollection>,
}

impl CachedEip712Provider {
    /// Collects EIP-712 canonical types for every root in parallel. Parses
    /// whatever it is given — it makes no assumptions about which roots are
    /// relevant.
    pub fn collect(
        roots: &[Eip712Root],
        import_resolver: &ImportResolver,
    ) -> Result<Self, CollectError> {
        let by_source = roots
            .par_iter()
            .map(
                |Eip712Root {
                     source,
                     path,
                     version,
                 }| {
                    collect_eip712_types_for_file(path, version, import_resolver)
                        .map(|collection| (source.clone(), collection))
                },
            )
            .collect::<Result<_, _>>()?;

        Ok(Self { by_source })
    }

    /// Looks up an EIP-712 canonical type by name within the scope of
    /// `source_path`.
    pub fn get_eip712_type(
        &self,
        source_path: &Path,
        type_name: &str,
    ) -> Result<Eip712Type, Eip712LookupError> {
        let collection =
            self.by_source
                .get(source_path)
                .ok_or(Eip712LookupError::UnknownSource {
                    source_path: source_path.to_string_lossy().into_owned(),
                })?;

        collection
            .get(type_name)
            .cloned()
            .map_err(|error| match error {
                Eip712CollectionLookupError::NotFound { type_name } => {
                    Eip712LookupError::NotFound {
                        source_path: source_path.to_string_lossy().into_owned(),
                        type_name,
                    }
                }
                Eip712CollectionLookupError::Rejected(error) => error.into(),
            })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AsyncEip712Error {
    #[error("Background collection failed: {0}")]
    CollectionFailed(#[from] CollectError),
    #[error(transparent)]
    LookupFailed(#[from] Eip712LookupError),
}

pub struct Eip712TypeRequest {
    source: PathBuf,
    type_name: String,
    response_sender: crossbeam_channel::Sender<Result<Eip712Type, AsyncEip712Error>>,
}

/// A cloneable, `Send + Sync` handle that collects EIP-712 types in the
/// background, parallelizing the per-root parses (via [`rayon`]). Queries block
/// until collection has finished.
#[derive(Clone, Debug)]
pub struct SharedEip712Provider {
    request_sender: mpsc::Sender<Eip712TypeRequest>,
    backend: Arc<AsyncEip712ProviderBackend>,
}

impl SharedEip712Provider {
    const THREAD_NAME_PREFIX: &'static str = "async-eip712-provider";

    pub fn collect(roots: Vec<Eip712Root>, import_resolver: ImportResolver) -> Result<Self, CollectError> {
        let provider = CachedEip712Provider::collect(&roots, &import_resolver)?;
    }

    /// Spawns a background thread that collects every root in parallel and then
    /// makes the resulting [`Eip712Provider`] available to queries.
    pub fn collect_in_background(roots: Vec<Eip712Root>, import_resolver: ImportResolver) -> Self {
        thread::Builder::new()
            .name("async-eip712-provider".to_owned())
            .spawn(move || {
                let provider = 

                let by_source = roots
                    .par_iter()
                    .map(|root| (root.source.clone(), collect_root(root, &import_resolver)))
                    .collect();
                let provider = CachedEip712Provider { by_source };

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
    pub fn get_eip712_type(
        &self,
        source: &Path,
        type_name: &str,
    ) -> Result<Eip712Type, AsyncEip712Error> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let request = Eip712TypeRequest {
            source: source.to_path_buf(),
            type_name: type_name.to_string(),
            response_sender: sender,
        };

        self.request_sender
            .send(request)
            .expect("EIP-712 provider request channel should be open");

        receiver
            .recv()
            .expect("EIP-712 provider response channel should be open")
    }
}

/// A wrapper type to hold the `JoinHandle` of the `AsyncEip712Provider`'s
/// background thread and ensure it is joined on drop.
#[derive(Debug)]
pub struct AsyncEip712ProviderBackend {
    background_thread: Option<thread::JoinHandle<()>>,
}

impl Drop for AsyncEip712ProviderBackend {
    fn drop(&mut self) {
        // The background thread should have finished by the time the backend is
        // dropped, but we join it here to be sure and to avoid a potential
        // panic if it tries to access the collection after it's been dropped.
        if let Some(background_thread) = self.background_thread.take() {
            background_thread
                .join()
                .expect("EIP-712 collector thread should join cleanly");
        }
    }
}

impl 

// NOTE (Default): `AsyncEip712Provider` intentionally has no `Default` impl —
// an EIP-712 provider is meaningless without the roots it covers.
// `CheatsConfigOptions` currently derives `Default` and is `..default()`-ed in
// two non-production spots (a `TestRunnerConfig` `Default` impl and an
// `edr_solidity_tests` test helper). Deciding how to satisfy that — give this
// an empty `Default`, model the field as `Option`/an enum, or drop `Default`
// and construct explicitly — is left open here on purpose.

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
        let provider =
            CachedEip712Provider::collect(&[root("relative/Root.sol")], &ImportResolver::default());

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
        assert!(provider
            .get(Path::new("never/collected.sol"), "Mail")
            .is_err());
    }

    #[test]
    fn sync_provider_resolves_mapped_imports() {
        let mut import_map = HashMap::new();
        import_map.insert(
            "@lib/Token.sol".to_string(),
            fixtures_root().join("mapped/lib/Token.sol"),
        );
        let provider = CachedEip712Provider::collect(
            &[root("mapped/Root.sol")],
            &ImportResolver::new(import_map),
        );

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
        let provider = SharedEip712Provider::new(
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
        let provider = Arc::new(SharedEip712Provider::new(
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
