//! Config-based channel enabling (`src/adapters/channel/loadEnabledChannels.ts`).
//!
//! The TS CHANNEL_LOADERS table is a fixed 15-entry record: iteration order,
//! the `cfg?.enabled` truthiness gate, the per-key `cfg`-to-options
//! extraction (absent optionals stay present-but-undefined properties), and
//! error-then-continue on a failed construction. The real channel classes
//! land with the A8.3-ch-* units, so each entry's constructor is a mount
//! point; an unregistered key enabled in config fails closed exactly like a
//! failed dynamic import and the loop moves on.

use std::{collections::HashMap, sync::Arc};

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::protocol::{ChannelAdapter, ChannelError};

/// `Object.entries(CHANNEL_LOADERS)` order.
pub const CHANNEL_LOADER_KEYS: [&str; 15] = [
    "telegram",
    "discord",
    "slack",
    "matrix",
    "mattermost",
    "signal",
    "whatsapp",
    "bluebubbles",
    "dingtalk",
    "wecomCallback",
    "email",
    "sms",
    "homeassistant",
    "apiServer",
    "webhook",
];

/// The 13 channel config keys not migrated to Rust (#1302 B, #1386): enabling
/// any of them must abort native startup. The set is the eleven
/// `CHANNEL_LOADER_KEYS` members plus `qq` and `wecom`, which TS builds
/// through `ChannelAdapterBundle` instead of the loader table. The order is
/// deterministic — reverse `CHANNEL_LOADER_KEYS` order, then `qq`, then
/// `wecom`; the first enabled key in this order is the one reported. Reverse
/// order on purpose: a table that repeats `CHANNEL_LOADER_KEYS`'s own
/// adjacent key pairs (e.g. `"telegram",\n    "discord",`) would make the
/// byte anchors other fixtures pin on that table ambiguous
/// (`channel-core`'s `config-order` mutation expects exactly one match).
pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[
    "homeassistant",
    "email",
    "dingtalk",
    "bluebubbles",
    "whatsapp",
    "signal",
    "mattermost",
    "matrix",
    "slack",
    "discord",
    "telegram",
    "qq",
    "wecom",
];

/// The two keys whose TS gate is strict. `qq` and `wecom` are built by
/// `ChannelAdapterBundle.createQQ` / `createWeCom`, which both start with
/// `if (settings?.enabled !== true) return undefined` — a config of
/// `{"enabled": 1}` or `{"enabled": "true"}` builds nothing and the TS
/// gateway starts normally. The other eleven keys are consumed by
/// `loadEnabledChannels`'s `if (!cfg?.enabled) continue`, i.e. JS truthiness.
/// Measured on the frozen baseline `cc7c4e65` over `true` / `1` / `"true"` /
/// `"1"` / `[]` / `{}` / falsy / absent: `specs/gateway-parity/diffs/
/// A8.3-CHANNEL-QQ-001.md` records the probe for the two keys above
/// (`createStartupAdapters`) and its sibling run for the eleven loader keys
/// (`loadEnabledChannels`). The seam sees whatever object the assembly unit
/// feeds, and both sides coerce `enabled` to a strict bool before their
/// gates (`config/snapshot.rs` here, `parseGatewayConfig`'s `booleanField` in
/// TS), so on a real config file the two gates cannot be told apart — the
/// raw-object split above is the seam contract, kept in that ledger caveat.
pub(crate) const STRICT_TRUE_CHANNEL_KEYS: &[&str] = &["qq", "wecom"];

/// The TS `enabled` gate for one key: strict `=== true` for the
/// `ChannelAdapterBundle` keys, JS truthiness (`js_truthy`) for the loader
/// table. Splitting the table here is what keeps the Rust rejection aligned
/// with the two different TS gates instead of imposing one on both.
fn rejects_enabled(key: &str, cfg: &JsValue) -> bool {
    if STRICT_TRUE_CHANNEL_KEYS.contains(&key) {
        matches!(cfg.get("enabled"), Some(JsValue::Bool(true)))
    } else {
        js_truthy(cfg.get("enabled"))
    }
}

/// `#1302 B`: a config that enables a channel Rust does not implement
/// refuses startup — never a silent skip, a downgrade to disabled, or a
/// fallback to the TS gateway. The gate reads the config object's own keys
/// (`wecom`/`qq` are absent from `CHANNEL_LOADER_KEYS`, so iterating the
/// loader table alone would miss them); `enabled` follows each key's own TS
/// gate — strict `=== true` for the two `ChannelAdapterBundle` keys, JS
/// truthiness for the loader table (see [`STRICT_TRUE_CHANNEL_KEYS`]) — and
/// `None` adapters is the TS
/// `if (!adapters) return []` no-configuration path. Production calls this
/// seam from `native_process/assembly.rs` before `NativeChannels` is
/// constructed (and therefore before any adapter construction or
/// `console.error` line); see also [`reject_unimplemented_bundle_channels`]
/// for the `weixin` / `feishu` keys this table cannot cover.
pub fn reject_unmigrated_channels(adapters: Option<&JsValue>) -> Result<(), ChannelError> {
    let Some(adapters) = adapters else {
        return Ok(());
    };
    for key in UNMIGRATED_CHANNEL_KEYS {
        let Some(cfg) = adapters.get(key) else {
            continue;
        };
        if rejects_enabled(key, cfg) {
            return Err(ChannelError::error(format!(
                "[adapters] channel \"{key}\" 未迁移到 Rust，需继续使用 TS gateway"
            )));
        }
    }
    Ok(())
}

/// The `ChannelAdapterBundle` keys that are not in [`CHANNEL_LOADER_KEYS`].
/// TS builds `weixin` and `feishu` through `ChannelAdapterBundle.createWeixin`
/// / `createFeishu` rather than the loader table, and neither is in
/// [`UNMIGRATED_CHANNEL_KEYS`] — the two bundle keys that are (`qq`, `wecom`)
/// already refuse through [`reject_unmigrated_channels`]. Without this list
/// the same silent skip would simply reappear under a different key name
/// (#1694 §7).
pub const BUNDLE_CHANNEL_KEYS: &[&str] = &["weixin", "feishu"];

/// The refusal reason both gates print. [`reject_unmigrated_channels`] keeps
/// its own literal on purpose: the frozen `channel-unmigrated`
/// negative-control fixture pins byte anchors on that exact line
/// (`rejection-silenced`, `rejection-message-loses-key`), and an interpolated
/// copy would make those anchors ambiguous. Both test modules assert the
/// exact text, so the two spellings cannot drift silently.
pub const UNMIGRATED_REASON: &str = "未迁移到 Rust，需继续使用 TS gateway";

/// `#1694 §7`: an enabled `ChannelAdapterBundle` key with no assembly
/// implementation refuses startup exactly like an unmigrated loader key —
/// never a silent skip, a downgrade to disabled, or a fallback to TS. The
/// assembly declares what it actually constructs (`implemented`); a key it
/// cannot build is rejected before any channel is constructed.
///
/// Both constructors gate on strict `=== true` (`ChannelAdapterBundle.ts:146`,
/// `createFeishu`), which is why this check does not use [`js_truthy`]: on a
/// raw object `{"enabled": 1}` builds nothing in TS and must not reject. The
/// production path feeds the parsed snapshot, where `config/snapshot.rs`
/// (`enabled_only`, `feishu`) has already collapsed `enabled` to a real bool,
/// so the two gates agree on every config file.
///
/// The message keeps [`reject_unmigrated_channels`]' phrase — from the
/// operator's side the instruction is the same: keep using the TS gateway.
pub fn reject_unimplemented_bundle_channels(
    adapters: Option<&JsValue>,
    implemented: &[&str],
) -> Result<(), ChannelError> {
    let Some(adapters) = adapters else {
        return Ok(());
    };
    for key in BUNDLE_CHANNEL_KEYS {
        let Some(cfg) = adapters.get(key) else {
            continue;
        };
        // Strict `=== true`, matching both bundle constructors' own gate. The
        // value is bound first so this line does not repeat the byte anchor the
        // frozen `channel-unmigrated` mutation controls pin on
        // `rejects_enabled` (that anchor must stay unique).
        let enabled = cfg.get("enabled");
        if !matches!(enabled, Some(JsValue::Bool(true))) {
            continue;
        }
        if implemented.contains(key) {
            continue;
        }
        return Err(ChannelError::error(format!(
            "[adapters] channel \"{key}\" {UNMIGRATED_REASON}"
        )));
    }
    Ok(())
}

/// One mount point: constructs the channel from its option object.
pub type ChannelAdapterConstructor =
    Arc<dyn Fn(&JsObject) -> Result<Arc<dyn ChannelAdapter>, ChannelError> + Send + Sync>;

#[derive(Default)]
pub struct ChannelLoaderMounts {
    creators: HashMap<&'static str, ChannelAdapterConstructor>,
}

impl ChannelLoaderMounts {
    pub fn register(&mut self, key: &'static str, creator: ChannelAdapterConstructor) {
        self.creators.insert(key, creator);
    }
    pub fn is_registered(&self, key: &str) -> bool {
        self.creators.contains_key(key)
    }
    fn creator(&self, key: &str) -> ChannelAdapterConstructor {
        self.creators.get(key).expect("registered key").clone()
    }
}

pub struct LoadEnabledChannelsOptions {
    pub pilot_home: Option<JsString>,
}

/// `loadEnabledChannels(adapters, options)`: synchronous like the TS body's
/// sequential `await` chain — constructions and error lines interleave in
/// table order. `console.error` lines are reported through `console_error`.
pub fn load_enabled_channels(
    adapters: Option<&JsValue>,
    options: &LoadEnabledChannelsOptions,
    mounts: &ChannelLoaderMounts,
    console_error: &mut dyn FnMut(String),
) -> Vec<Arc<dyn ChannelAdapter>> {
    let Some(adapters) = adapters else {
        return Vec::new();
    };
    let mut channels = Vec::new();
    for key in CHANNEL_LOADER_KEYS {
        let Some(cfg) = adapters.get(key) else {
            continue;
        };
        if !js_truthy(cfg.get("enabled")) {
            continue;
        }
        let constructor = match mounts.is_registered(key).then(|| mounts.creator(key)) {
            Some(constructor) => constructor,
            None => {
                console_error(format!(
                    "[adapters] Failed to load channel \"{key}\": Error: channel module not registered: {key}"
                ));
                continue;
            }
        };
        let built = build_options(key, cfg, options);
        match constructor(&built) {
            Ok(channel) => channels.push(channel),
            Err(error) => console_error(format!(
                "[adapters] Failed to load channel \"{key}\": {}",
                error.to_js_string()
            )),
        }
    }
    channels
}

/// JavaScript truthiness for the `cfg?.enabled` gate (and, since #1334, the
/// webhook channel's `route.secret || globalSecret` and extractText gates).
pub(super) fn js_truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) | Some(JsValue::Bool(false)) => false,
        Some(JsValue::Bool(true)) | Some(JsValue::Object(_)) | Some(JsValue::Array(_)) => true,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
    }
}

fn undefined() -> JsValue {
    JsValue::Undefined
}

/// `cfg.token`, `cfg.webhookUrl`, ... — the property when present, an
/// undefined value otherwise.
fn field(cfg: &JsValue, name: &str) -> JsValue {
    cfg.get(name).cloned().unwrap_or_else(undefined)
}

fn extra_field(cfg: &JsValue, name: &str) -> JsValue {
    cfg.get("extra")
        .and_then(|extra| extra.as_object())
        .and_then(|extra| extra.get(name))
        .cloned()
        .unwrap_or_else(undefined)
}

fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

/// The exact option object each TS loader literal builds, insertion order
/// included; absent config values become present-but-undefined properties.
fn build_options(key: &str, cfg: &JsValue, options: &LoadEnabledChannelsOptions) -> JsObject {
    let mut built = JsObject::new();
    let insert = |object: &mut JsObject, name: &str, value: JsValue| {
        object.insert(name.into(), value);
    };
    match key {
        "telegram" => {
            insert(&mut built, "token", field(cfg, "token"));
            insert(&mut built, "webhookUrl", field(cfg, "webhookUrl"));
        }
        "discord" => {
            insert(&mut built, "token", field(cfg, "token"));
        }
        "slack" => {
            insert(&mut built, "botToken", field(cfg, "token"));
            insert(&mut built, "appToken", extra_field(cfg, "appToken"));
        }
        "matrix" => {
            insert(&mut built, "accessToken", field(cfg, "token"));
            insert(&mut built, "homeserver", extra_field(cfg, "homeserver"));
            insert(&mut built, "userId", extra_field(cfg, "userId"));
            insert(
                &mut built,
                "pilotHome",
                options
                    .pilot_home
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or_else(undefined),
            );
        }
        "mattermost" => {
            insert(&mut built, "token", field(cfg, "token"));
            insert(&mut built, "serverUrl", extra_field(cfg, "serverUrl"));
            insert(&mut built, "teamId", extra_field(cfg, "teamId"));
        }
        "signal" => {
            insert(&mut built, "restUrl", extra_field(cfg, "restUrl"));
            insert(&mut built, "account", extra_field(cfg, "account"));
        }
        "whatsapp" => {
            insert(&mut built, "bridgePath", extra_field(cfg, "bridgePath"));
            insert(&mut built, "bridgeUrl", extra_field(cfg, "bridgeUrl"));
        }
        "bluebubbles" => {
            insert(&mut built, "serverUrl", extra_field(cfg, "serverUrl"));
            let token = field(cfg, "token");
            // `cfg.token ?? cfg.extra?.password`: nullish token falls back.
            insert(
                &mut built,
                "password",
                if nullish(&token) {
                    extra_field(cfg, "password")
                } else {
                    token
                },
            );
        }
        "dingtalk" => {
            insert(&mut built, "clientId", extra_field(cfg, "clientId"));
            insert(&mut built, "clientSecret", extra_field(cfg, "clientSecret"));
        }
        "wecomCallback" => {
            insert(&mut built, "corpId", extra_field(cfg, "corpId"));
            insert(&mut built, "agentId", extra_field(cfg, "agentId"));
            insert(&mut built, "secret", extra_field(cfg, "secret"));
            insert(&mut built, "token", field(cfg, "token"));
            insert(
                &mut built,
                "encodingAesKey",
                extra_field(cfg, "encodingAesKey"),
            );
            insert(&mut built, "port", extra_field(cfg, "port"));
        }
        "email" | "sms" => {
            insert(&mut built, "extra", field(cfg, "extra"));
        }
        "homeassistant" => {
            insert(&mut built, "url", extra_field(cfg, "url"));
            insert(&mut built, "token", field(cfg, "token"));
        }
        "apiServer" => {
            insert(&mut built, "port", extra_field(cfg, "port"));
            insert(&mut built, "apiKey", field(cfg, "apiKey"));
        }
        "webhook" => {
            insert(&mut built, "port", extra_field(cfg, "port"));
            insert(&mut built, "secret", extra_field(cfg, "secret"));
        }
        _ => unreachable!("key comes from CHANNEL_LOADER_KEYS"),
    }
    built
}
