//! The environment keys the gateway's proxy configuration reads, re-exported
//! from the production constant (`runtime::model::http::PROXY_ENV_KEYS`), so
//! child-process env scrubbing cannot drift from what production reads.

pub use pilotdeck_gateway::runtime::model::http::PROXY_ENV_KEYS;
