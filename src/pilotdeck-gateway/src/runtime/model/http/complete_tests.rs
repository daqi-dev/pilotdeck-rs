use super::complete::is_retryable_request_error;
use super::tests::{RETRY_WORDS_ORACLE, oracle_text};
use super::*;

/// The complete()/`isRetryableRequestError` sibling list differs deliberately
/// from the stream list (etimedout in, aborted out). The fixture records the
/// baseline source list; the Rust classifier must match it. The raw branch is
/// not reachable through `complete()` because `sendProviderRequest` wraps every
/// transport error in `ModelProviderError` first, so this is source-derived
/// rather than a runtime golden (see README).
#[test]
fn complete_retry_message_list_matches_the_ts_request_classifier_source() {
    let oracle = crate::test_baseline::oracle(RETRY_WORDS_ORACLE);
    let words = super::super::count_body::array(field(
        field(&oracle, "sourceLists"),
        "isRetryableRequestError",
    ));
    assert_eq!(words.len(), 8, "request-list word count");
    for word in words {
        let message = oracle_text(word);
        let error = ModelStreamError::Thrown {
            name: "Error".into(),
            message: message.clone().into(),
        };
        assert!(
            is_retryable_request_error(&error),
            "request retry word {message:?}"
        );
    }
    for message in ["dns", "timed out", "connection reset by peer", "aborted"] {
        let error = ModelStreamError::Thrown {
            name: "Error".into(),
            message: message.into(),
        };
        assert!(
            !is_retryable_request_error(&error),
            "request non-word {message:?}"
        );
    }
}
