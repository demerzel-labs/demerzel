//! Provider adapters for the Demerzel harness.
//!
//! The provider contract — the trait, capabilities, request shape, and
//! stream events — lives in `demerzel-core` next to the loop that
//! consumes it. This crate holds the adapters and re-exports the
//! contract, so adapter code and adapter users import one crate.

pub mod anthropic;
pub mod auth;
pub mod endpoint;
pub mod oauth;
pub mod openai_codex;
pub mod openai_compat;
pub mod openai_responses;
mod retry;
mod sse;

pub use demerzel_core::{
    BoxFuture, Capabilities, EventStream, Provider, ProviderError, Request, StreamEvent,
    ToolDefinition,
};

/// The adapter ids this build ships: the values of a provider
/// instance's `kind`. A slice, so a new adapter never changes the
/// item's type.
pub const PROVIDER_IDS: &[&str] = &[
    openai_codex::PROVIDER_ID,
    anthropic::PROVIDER_ID,
    openai_compat::PROVIDER_ID,
    openai_responses::PROVIDER_ID,
];

/// How an instance sets the thinking and effort fields: the value of
/// the `thinking` config key, for an adapter with a thinking switch
/// (see [`AdapterRules::thinking_switch`]). The adapter maps it to its
/// wire fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ThinkingMode {
    /// Adaptive thinking, and the request's effort.
    #[default]
    Adaptive,
    /// Thinking disabled and no effort field, whatever the request's
    /// effort. For a model without adaptive thinking or effort; a
    /// model that requires thinking refuses it.
    Off,
}

impl ThinkingMode {
    /// The value names, for help and error text.
    pub const NAMES: &'static str = "adaptive, off";

    /// The value's config name.
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingMode::Adaptive => "adaptive",
            ThinkingMode::Off => "off",
        }
    }

    /// Whether requests carry the effort field.
    pub fn sends_effort(self) -> bool {
        self == ThinkingMode::Adaptive
    }
}

impl std::str::FromStr for ThinkingMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [ThinkingMode::Adaptive, ThinkingMode::Off]
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or_else(|| format!("must be one of {}, not `{value}`", ThinkingMode::NAMES))
    }
}

impl std::fmt::Display for ThinkingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the harness checks about an adapter before it builds one: the
/// config an instance must supply and the request fields the adapter
/// takes without a setting. One table, so an adapter states its rules
/// once and every check reads them from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AdapterRules {
    /// The adapter has no default endpoint: an instance names
    /// `base_url`.
    pub needs_endpoint: bool,
    /// The adapter receives the default effort level. Without this,
    /// only a level the user set goes on the wire.
    pub implicit_effort: bool,
    /// An instance can name its key variable in `api_key_env`.
    pub key_variable: bool,
    /// The adapter has credentials of its own: a login or a key that
    /// serves every instance at the adapter's default endpoint.
    pub own_credentials: bool,
    /// An instance can set `thinking`: the adapter can turn its
    /// thinking and effort fields off.
    pub thinking_switch: bool,
}

/// The rules of adapter `kind`; `None` for an id this build lacks.
pub fn adapter_rules(kind: &str) -> Option<AdapterRules> {
    let rules = match kind {
        openai_codex::PROVIDER_ID => AdapterRules {
            needs_endpoint: false,
            implicit_effort: true,
            key_variable: false,
            own_credentials: true,
            thinking_switch: false,
        },
        anthropic::PROVIDER_ID => AdapterRules {
            needs_endpoint: false,
            implicit_effort: true,
            key_variable: true,
            own_credentials: true,
            thinking_switch: true,
        },
        openai_compat::PROVIDER_ID | openai_responses::PROVIDER_ID => AdapterRules {
            needs_endpoint: true,
            implicit_effort: false,
            key_variable: true,
            own_credentials: false,
            thinking_switch: false,
        },
        _ => return None,
    };
    Some(rules)
}

/// The default model of an adapter; `None` for an unknown id and for
/// an adapter without a default, such as `openai-compat` or
/// `openai-responses`, whose instance must name its model.
pub fn default_model(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        openai_codex::PROVIDER_ID => Some(openai_codex::DEFAULT_MODEL),
        anthropic::PROVIDER_ID => Some(anthropic::DEFAULT_MODEL),
        _ => None,
    }
}

/// The context window of a shipped provider's model, in tokens, from
/// the adapter's static catalog. `None` when the provider id or the
/// model is not in a catalog. The `context_window` config key
/// overrides the catalog and covers the models no catalog names.
pub fn context_window(provider_id: &str, model: &str) -> Option<u64> {
    let table = match provider_id {
        openai_codex::PROVIDER_ID => openai_codex::CONTEXT_WINDOWS,
        anthropic::PROVIDER_ID => anthropic::CONTEXT_WINDOWS,
        _ => return None,
    };
    let row = table.iter().find(|(name, _)| *name == model);
    row.map(|(_, window)| *window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogs_name_the_default_models_and_nothing_unknown() {
        for id in PROVIDER_IDS {
            if let Some(model) = default_model(id) {
                assert!(context_window(id, model).is_some(), "{id}");
            }
            assert_eq!(context_window(id, "not-a-model"), None, "{id}");
        }
        // The endpoint adapters have no default and no catalog.
        for id in [openai_compat::PROVIDER_ID, openai_responses::PROVIDER_ID] {
            assert_eq!(default_model(id), None);
            assert!(adapter_rules(id).unwrap().needs_endpoint);
        }
    }

    #[test]
    fn every_adapter_states_its_rules() {
        for id in PROVIDER_IDS {
            let rules = adapter_rules(id).unwrap();
            // An adapter without a default endpoint has no default
            // model and no credentials of its own.
            if rules.needs_endpoint {
                assert_eq!(default_model(id), None, "{id}");
                assert!(!rules.own_credentials, "{id}");
            }
        }
        assert_eq!(adapter_rules("not-an-adapter"), None);
        assert_eq!(context_window("not-a-provider", "gpt-5.6-sol"), None);
    }
}
