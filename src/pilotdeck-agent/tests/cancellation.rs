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
