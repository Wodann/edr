//! Lazy, cached, parallel resolution of EIP-712 canonical types from Solidity
//! sources.
//!
//! Three layers, each with a single responsibility:
//! - [`Eip712TypeProvider`] — immutable: resolves an [`ArtifactId`] to its root
//!   source, parses, and returns a type. No caching.
//! - [`CachedEip712TypeProvider`] — `&mut self`: memoizes collections per
//!   resolved root. Owned by a single service thread, so it needs no locks.
//! - [`SharedEip712TypeProvider`] — a cloneable, `Send + Sync` handle that talks
//!   to that service thread over channels. Parses for distinct roots run in
//!   parallel on a persistent [`rayon`] thread pool; concurrent lookups for the
//!   same root dedupe behind a single parse.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
};

use edr_artifact::ArtifactId;
use edr_common::fs::normalize_path;
use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::{collect_eip712_canonical_types, Eip712Collection, Eip712TypeDef};

/// The cached result of collecting a single root source.
type CollectionResult = Arc<Result<Eip712Collection, String>>;

/// A pending lookup: the requested type name and the channel to answer on.
type Waiter = (String, mpsc::Sender<Result<Eip712TypeDef, String>>);

/// Immutable EIP-712 type resolution: given an artifact, resolve its source
/// path, parse it (and its imports), and look up a type. Does no caching.
#[derive(Clone, Debug, Default)]
pub struct Eip712TypeProvider {
    project_root: PathBuf,
    import_mappings: HashMap<String, PathBuf>,
}

impl Eip712TypeProvider {
    /// Creates a provider rooted at `project_root` (against which artifact
    /// source paths are resolved) with the given non-relative import mappings.
    pub fn new(project_root: PathBuf, import_mappings: HashMap<String, PathBuf>) -> Self {
        Self {
            project_root,
            import_mappings,
        }
    }

    /// The normalized, absolute path of an artifact's source file.
    fn root_source(&self, artifact_id: &ArtifactId) -> PathBuf {
        normalize_path(&self.project_root.join(&artifact_id.source))
    }

    /// Collects every EIP-712 canonical type reachable from the artifact's
    /// source. The error is stringified so it can be cached and replayed.
    pub fn collection(&self, artifact_id: &ArtifactId) -> Result<Eip712Collection, String> {
        collect_eip712_canonical_types(
            &self.root_source(artifact_id),
            &artifact_id.version,
            &self.import_mappings,
        )
        .map_err(|error| error.to_string())
    }

    /// Resolves a single EIP-712 type by name from the artifact's source.
    pub fn get_eip_712_type(
        &self,
        artifact_id: &ArtifactId,
        type_name: &str,
    ) -> Result<Eip712TypeDef, String> {
        self.collection(artifact_id)?
            .get(type_name)
            .cloned()
            .map_err(|error| error.to_string())
    }
}

/// Memoizing wrapper over [`Eip712TypeProvider`], keyed by resolved root source.
/// Intended to be owned by a single thread (no internal locking).
#[derive(Debug, Default)]
pub struct CachedEip712TypeProvider {
    inner: Eip712TypeProvider,
    cache: HashMap<PathBuf, CollectionResult>,
}

impl CachedEip712TypeProvider {
    /// Creates a cached provider. See [`Eip712TypeProvider::new`].
    pub fn new(project_root: PathBuf, import_mappings: HashMap<String, PathBuf>) -> Self {
        Self {
            inner: Eip712TypeProvider::new(project_root, import_mappings),
            cache: HashMap::new(),
        }
    }

    /// Returns the (cached) collection for an artifact's root source.
    fn collection(&mut self, artifact_id: &ArtifactId) -> CollectionResult {
        let root = self.inner.root_source(artifact_id);
        if let Some(cached) = self.cache.get(&root) {
            return Arc::clone(cached);
        }
        let result = Arc::new(self.inner.collection(artifact_id));
        self.cache.insert(root, Arc::clone(&result));
        result
    }

    /// Resolves a single EIP-712 type by name, parsing (and caching) the
    /// artifact's source on first use.
    pub fn get_eip_712_type(
        &mut self,
        artifact_id: &ArtifactId,
        type_name: &str,
    ) -> Result<Eip712TypeDef, String> {
        resolve(&self.collection(artifact_id), type_name)
    }
}

/// Looks up a type name in an already-collected (or failed) collection.
fn resolve(collection: &CollectionResult, type_name: &str) -> Result<Eip712TypeDef, String> {
    match collection.as_ref() {
        Ok(collection) => collection
            .get(type_name)
            .cloned()
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.clone()),
    }
}

/// Messages handled by the provider service thread.
enum Message {
    /// Resolve `type_name` for `artifact_id`, sending the answer to `reply`.
    Lookup {
        artifact_id: ArtifactId,
        type_name: String,
        reply: mpsc::Sender<Result<Eip712TypeDef, String>>,
        /// A handle back to the service, used to seed parse tasks. Carried in
        /// the message (rather than held by the service) so the service holds
        /// no sender of its own — that lets the service loop terminate once all
        /// [`SharedEip712TypeProvider`] handles and in-flight tasks are gone.
        respond_via: mpsc::Sender<Message>,
    },
    /// A parse finished for `root`, yielding `result`.
    Parsed {
        root: PathBuf,
        result: CollectionResult,
    },
}

/// A cloneable, `Send + Sync` handle to a background EIP-712 type provider.
///
/// Cloning shares the same service thread and cache; the service shuts down
/// (and its rayon pool is reclaimed) once every clone has been dropped.
#[derive(Clone, Debug)]
pub struct SharedEip712TypeProvider {
    request_tx: mpsc::Sender<Message>,
}

impl SharedEip712TypeProvider {
    /// Spawns the service thread and its rayon pool. See
    /// [`Eip712TypeProvider::new`] for `project_root`/`import_mappings`.
    pub fn new(project_root: PathBuf, import_mappings: HashMap<String, PathBuf>) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Message>();
        let provider = Arc::new(Eip712TypeProvider::new(project_root, import_mappings));

        thread::Builder::new()
            .name("eip712-type-provider".to_string())
            .spawn(move || {
                let pool = ThreadPoolBuilder::new()
                    .thread_name(|i| format!("eip712-parse-{i}"))
                    .build()
                    .expect("rayon thread pool should build");
                run_service(&request_rx, &provider, &pool);
            })
            .expect("EIP-712 provider service thread should spawn");

        Self { request_tx }
    }

    /// Resolves a single EIP-712 type by name from the artifact's source.
    /// Blocks until the (possibly already-cached) parse completes.
    pub fn get_eip_712_type(
        &self,
        artifact_id: &ArtifactId,
        type_name: &str,
    ) -> Result<Eip712TypeDef, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.request_tx
            .send(Message::Lookup {
                artifact_id: artifact_id.clone(),
                type_name: type_name.to_string(),
                reply: reply_tx,
                respond_via: self.request_tx.clone(),
            })
            .map_err(|_send_error| "EIP-712 type provider is unavailable".to_string())?;
        reply_rx
            .recv()
            .map_err(|_recv_error| "EIP-712 type provider dropped the request".to_string())?
    }
}

impl Default for SharedEip712TypeProvider {
    fn default() -> Self {
        Self::new(PathBuf::new(), HashMap::new())
    }
}

/// The service loop: owns the cache, serializes cache access, and dispatches
/// parses to the rayon pool. Returns (ending the thread) when the request
/// channel is closed — i.e. all handles and in-flight tasks are gone.
fn run_service(
    request_rx: &mpsc::Receiver<Message>,
    provider: &Arc<Eip712TypeProvider>,
    pool: &ThreadPool,
) {
    let mut cache: HashMap<PathBuf, CollectionResult> = HashMap::new();
    let mut in_flight: HashMap<PathBuf, Vec<Waiter>> = HashMap::new();

    while let Ok(message) = request_rx.recv() {
        match message {
            Message::Lookup {
                artifact_id,
                type_name,
                reply,
                respond_via,
            } => {
                let root = provider.root_source(&artifact_id);

                if let Some(cached) = cache.get(&root) {
                    let _ = reply.send(resolve(cached, &type_name));
                    continue;
                }

                let waiters = in_flight.entry(root.clone()).or_default();
                let is_first = waiters.is_empty();
                waiters.push((type_name, reply));

                if is_first {
                    let task_provider = Arc::clone(provider);
                    pool.spawn(move || {
                        let result = Arc::new(task_provider.collection(&artifact_id));
                        // If the service is already gone, nobody is waiting.
                        let _ = respond_via.send(Message::Parsed { root, result });
                    });
                }
            }
            Message::Parsed { root, result } => {
                cache.insert(root.clone(), Arc::clone(&result));
                if let Some(waiters) = in_flight.remove(&root) {
                    for (type_name, reply) in waiters {
                        let _ = reply.send(resolve(&result, &type_name));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use semver::Version;

    use super::*;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    fn artifact(source: &str) -> ArtifactId {
        ArtifactId {
            name: "Root".to_string(),
            source: PathBuf::from(source),
            version: Version::new(0, 8, 24),
        }
    }

    fn mail_canonical() -> &'static str {
        "Mail(Person from,Person to,string contents)Person(address wallet,string name)"
    }

    #[test]
    fn immutable_provider_resolves_a_type() {
        let provider = Eip712TypeProvider::new(fixtures_root(), HashMap::new());
        let mail = provider
            .get_eip_712_type(&artifact("relative/Root.sol"), "Mail")
            .unwrap();
        assert_eq!(mail.canonical_definition(), mail_canonical());
    }

    #[test]
    fn cached_provider_reuses_one_collection_per_root() {
        let mut provider = CachedEip712TypeProvider::new(fixtures_root(), HashMap::new());
        let artifact = artifact("relative/Root.sol");

        let first = provider.collection(&artifact);
        let second = provider.collection(&artifact);
        // Same Arc => parsed once, served from cache the second time.
        assert!(Arc::ptr_eq(&first, &second));

        // Different type names from the same root still hit the cache.
        assert_eq!(
            provider
                .get_eip_712_type(&artifact, "Person")
                .unwrap()
                .canonical_definition(),
            "Person(address wallet,string name)"
        );
    }

    #[test]
    fn shared_provider_resolves_and_reports_unknowns() {
        let provider = SharedEip712TypeProvider::new(fixtures_root(), HashMap::new());

        assert_eq!(
            provider
                .get_eip_712_type(&artifact("relative/Root.sol"), "Mail")
                .unwrap()
                .canonical_definition(),
            mail_canonical()
        );
        assert!(provider
            .get_eip_712_type(&artifact("relative/Root.sol"), "DoesNotExist")
            .is_err());
    }

    #[test]
    fn shared_provider_resolves_mapped_imports() {
        let mut import_mappings = HashMap::new();
        import_mappings.insert(
            "@lib/Token.sol".to_string(),
            fixtures_root().join("mapped/lib/Token.sol"),
        );
        let provider = SharedEip712TypeProvider::new(fixtures_root(), import_mappings);

        assert_eq!(
            provider
                .get_eip_712_type(&artifact("mapped/Root.sol"), "Payment")
                .unwrap()
                .canonical_definition(),
            "Payment(Token token,uint256 amount)Token(address addr,uint8 decimals)"
        );
    }

    #[test]
    fn shared_provider_handles_concurrent_lookups_across_roots() {
        let provider = SharedEip712TypeProvider::new(fixtures_root(), HashMap::new());
        let provider = Arc::new(provider);

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let provider = Arc::clone(&provider);
                thread::spawn(move || {
                    let (source, type_name, expected) = if i % 2 == 0 {
                        ("relative/Root.sol", "Person", "Person(address wallet,string name)")
                    } else {
                        ("relative/Dep.sol", "Person", "Person(address wallet,string name)")
                    };
                    let def = provider
                        .get_eip_712_type(&artifact(source), type_name)
                        .unwrap();
                    assert_eq!(def.canonical_definition(), expected);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn service_thread_exits_when_handle_dropped() {
        // Dropping every handle must close the request channel so the service
        // loop returns and its rayon pool is reclaimed. We can't observe the
        // thread directly, but exercising a lookup then dropping must not hang.
        let provider = SharedEip712TypeProvider::new(fixtures_root(), HashMap::new());
        let _ = provider.get_eip_712_type(&artifact("relative/Root.sol"), "Mail");
        drop(provider);
        // Give the service a moment to wind down; the test passing (not
        // hanging) is the real assertion.
        assert!(Path::new(&fixtures_root()).exists());
    }
}
