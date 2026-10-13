use pilotdeck_agent::ports::{CancelCause, Cancellation};
use std::time::{Duration, Instant};

#[test]
fn later_observation_and_duplicate_cancel_cannot_extend_the_first_budget() {
    let cancellation = Cancellation::default();
    let grace = Duration::from_secs(5);
    let before = Instant::now();
    assert!(cancellation.cancel(CancelCause::Requested("first".into())));
    let after = Instant::now();
    let bound = cancellation.reconciliation_deadline(grace).unwrap();
    assert!(bound >= before + grace && bound <= after + grace);
    assert!(!cancellation.cancel_with_reconciliation_deadline(
        CancelCause::Deadline,
        after + Duration::from_secs(60),
    ));
    assert_eq!(cancellation.reconciliation_deadline(grace), Some(bound));
    assert_eq!(
        cancellation.cause(),
        Some(CancelCause::Requested("first".into()))
    );
}

#[test]
fn terminal_owner_fixed_deadline_survives_later_observation() {
    let cancellation = Cancellation::default();
    let expired = Instant::now() - Duration::from_secs(1);
    assert!(
        cancellation
            .cancel_with_reconciliation_deadline(CancelCause::Requested("finish".into()), expired,)
    );
    assert_eq!(
        cancellation.reconciliation_deadline(Duration::from_secs(5)),
        Some(expired)
    );
    assert!(!cancellation.cancel(CancelCause::OwnerRetired));
    assert_eq!(
        cancellation.wait_until(Instant::now()),
        Some(CancelCause::Requested("finish".into()))
    );
}

#[test]
fn cleanup_total_bound_is_independent_and_cannot_gain_another_grace() {
    let parent = Cancellation::default();
    parent.cancel(CancelCause::Deadline);
    let bound = Instant::now() - Duration::from_secs(1);
    let child = parent.bounded_cleanup_scope(bound);
    let grace = Duration::from_secs(5);
    assert!(
        child.cause().is_none(),
        "cleanup ignores the execution soft stop"
    );
    assert_eq!(child.reconciliation_deadline(grace), Some(bound));
    assert!(
        child.cancel_with_reconciliation_deadline(CancelCause::Deadline, Instant::now() + grace,)
    );
    assert_eq!(child.reconciliation_deadline(grace), Some(bound));
    assert!(!child.cancel(CancelCause::Requested("duplicate".into())));
    assert_eq!(child.reconciliation_deadline(grace), Some(bound));
    parent.cancel(CancelCause::OwnerRetired);
    assert_eq!(child.hard_stop_cause(), Some(CancelCause::OwnerRetired));
}
