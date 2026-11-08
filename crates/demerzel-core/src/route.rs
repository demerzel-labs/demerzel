//! Model routing: the target a model call goes to, and the host that
//! maps an instance name to its provider.
//!
//! A target names a provider instance and a model. The route holds the
//! targets of the main loop: the head, then the fallback chain. The
//! engine tries them in order, as [`Route`] describes, and asks the
//! [`ProviderHost`] for each one's provider, so the capabilities, the
//! adapter, and the effort come from the target it calls.

use crate::provider::{Effort, Provider, ProviderError};

/// One model target: a provider instance and the model it serves.
/// Non-exhaustive: build one with [`Target::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Target {
    /// The provider instance name, e.g. "local" for a
    /// `[providers.local]` table. A shipped adapter's implicit
    /// instance carries the adapter id.
    pub instance: String,
    pub model: String,
}

impl Target {
    pub fn new(instance: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            instance: instance.into(),
            model: model.into(),
        }
    }
}

/// The targets of the main loop's model calls: the head, then the
/// tail. A turn's first model call tries the head first. A failure
/// that another endpoint can avoid, before the first stream event
/// reaches the screen, moves the call to the next target, and the
/// turn's later calls start at the target that answered. The engine
/// names the failures that move a call. Non-exhaustive: build one
/// with [`Route::new`] and set the tail.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
    /// The first target of each model call. The meta records name it.
    pub head: Target,
    /// The fallback targets, in order.
    pub tail: Vec<Target>,
}

impl Route {
    /// A route with `head` alone.
    pub fn new(head: Target) -> Self {
        Self {
            head,
            tail: Vec::new(),
        }
    }

    /// Every target in the order a model call tries them: the head,
    /// then the tail.
    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        std::iter::once(&self.head).chain(&self.tail)
    }
}

/// Maps a provider instance name to its provider. The engine asks at
/// each model call, so one host serves every target of a route. An
/// implementation builds a provider on first use and keeps it for the
/// host's life; a failed build is not kept, so the next call retries.
pub trait ProviderHost: Send + Sync {
    /// The provider of instance `instance`. An error names why it
    /// does not build: a missing credential, or an instance the host
    /// does not know.
    fn provider(&self, instance: &str) -> Result<&dyn Provider, ProviderError>;

    /// The adapter id behind `instance`, without a build: no
    /// credential resolves. `None` for an instance the host does not
    /// know.
    fn kind(&self, instance: &str) -> Option<&str>;

    /// The reasoning effort that requests to `instance` carry when
    /// the session sets no level of its own: the level config gives
    /// the instance, or `None` for the provider's default. The
    /// default implementation sends none.
    fn effort(&self, instance: &str) -> Option<Effort> {
        let _ = instance;
        None
    }
}

/// A host over one provider that serves every instance name: the host
/// of a run with one provider, such as a test or a single-adapter
/// embedder.
#[derive(Clone, Copy)]
pub struct SingleProvider<'a>(pub &'a dyn Provider);

impl ProviderHost for SingleProvider<'_> {
    fn provider(&self, _instance: &str) -> Result<&dyn Provider, ProviderError> {
        Ok(self.0)
    }

    fn kind(&self, _instance: &str) -> Option<&str> {
        Some(self.0.id())
    }
}
