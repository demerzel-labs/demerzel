//! Model routing: the target a model call goes to, and the host that
//! maps an instance name to its provider.
//!
//! A target names a provider instance and a model. The route holds the
//! targets of the main loop; the engine reads the head target at each
//! model call and asks the [`ProviderHost`] for its provider, so the
//! capabilities and the adapter come from the target it calls.

use crate::provider::{Provider, ProviderError};

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

/// The targets of the main loop's model calls. The head target serves
/// every call. Non-exhaustive: build one with [`Route::new`], so a
/// fallback chain behind the head stays additive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Route {
    pub head: Target,
}

impl Route {
    /// A route with `head` alone.
    pub fn new(head: Target) -> Self {
        Self { head }
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
