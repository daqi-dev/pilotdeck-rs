//! Gateway-owned date/TZ parsing around the shared original error normalizer.
pub use pilotdeck_model_anthropic::errors::NetworkFetchError;
#[cfg(test)]
pub(crate) use pilotdeck_model_anthropic::errors::retry_after_from_message;
pub(crate) use pilotdeck_model_anthropic::errors::{
    ErrorInput, is_premature_stream_close, normalize_model_error, set_retry_after,
};
use pilotdeck_protocol::JsString;
/// `parseRetryAfterHeader` (protocol/errors.ts). Delta-seconds follow JS
/// `Number`; every other value goes through the full Node22 `Date.parse`
/// dialect set — IMF-fixdate, RFC 850, asctime, ISO and the V8-lenient forms
/// — shared with the execution clock (`pilotdeck_transport::system_clock`,
/// #360-2). A wall time without a timezone resolves against the process
/// timezone through the admitted system TZif subset — the same
/// three-identifier + SHA gate as conservative stats retention
/// (`stats::admitted_local_to_utc`, #360-2 fix-r2), not an arbitrary readable
/// TZif. Node also parses such values in environments that gate does not
/// admit (POSIX TZ rules, custom TZ paths, unreadable or unpaired tzdata,
/// host-dependent aliases like `TZ=localtime`), where this parser counts the
/// value as unparseable and the header is ignored. That retained TZ-subset
/// difference is registered by
/// `specs/gateway-parity/diffs/A6.4-PROVIDER-RETRY-AFTER-TZ-001.md` for every
/// consumer of this entry, not only the provider stream retry that reads the
/// header in `http.rs` (`streamModel.ts:231` on the TS side): the shared
/// `retry_after_header_ms` callers — the MCP HTTP retry delay, the web
/// network retry delay, and the web fetcher's `retryAfterMs` error field
/// (rendered as `Retry-After: …ms.` by the web_fetch failure text) — where TS
/// resolves through `network/fetch.ts`'s own identical
/// `parseRetryAfterHeader` copy or the protocol one. The entry follows the
/// same TZif subset as A6.4-STATS-RETENTION-001 but is a separate,
/// unapproved entry — the stats entry's scope is retention only.
pub(crate) fn retry_after_header(value: Option<&str>, now_ms: f64) -> Option<f64> {
    pilotdeck_model_anthropic::errors::retry_after_header_parsed(
        value,
        now_ms,
        &admitted_node22_parse,
    )
}

/// The production parse closure: the Node22 dialect parser with the shared
/// admitted-local resolver. Fail-closed gate errors (unverified TZif, alias
/// like `TZ=localtime`) make the whole value unparseable, never a UTC guess.
fn admitted_node22_parse(text: &JsString) -> Result<Option<f64>, JsString> {
    pilotdeck_transport::system_clock::parse_node22_date_with_local(
        text,
        &crate::runtime::model::stats::admitted_local_to_utc,
    )
}

/// The injected-parse seam for timezone-dependent dialects: the production
/// entry resolves local wall times against the process timezone, so unit
/// fixtures pin the local resolver instead of the ambient one (the same
/// policy `parse_node22_date_with_local` itself exposes).
#[cfg(test)]
pub(super) fn retry_after_header_with_local<L>(
    value: Option<&str>,
    now_ms: f64,
    local: &L,
) -> Option<f64>
where
    L: Fn(i64) -> Result<i64, JsString> + ?Sized,
{
    pilotdeck_model_anthropic::errors::retry_after_header_parsed(value, now_ms, &|text| {
        pilotdeck_transport::system_clock::parse_node22_date_with_local(text, &|wall| local(wall))
    })
}
