use super::*;
use std::{future::Future, task::Poll};

#[tokio::test(start_paused = true)]
async fn ordinary_completion_waits_past_publication_budget_in_native_and_legacy() {
    let closing_budget = Duration::from_millis(100);
    for budgets in [
        SubmissionBudgets::default(),
        SubmissionBudgets::native_connection(closing_budget),
    ] {
        let (send, receiver) = mpsc::unbounded_channel();
        let (consumed, acknowledged) = oneshot::channel();
        let (settle, completion) = oneshot::channel();
        let mut submission = TurnSubmission {
            receiver,
            consumed: Some(consumed),
            result: Some(completion),
            completion_wait_budget: budgets.completion_wait_budget,
        };
        send.send(Message::Event(object([
            ("type", "turn_completed".into()),
            ("finishReason", "completed".into()),
        ])))
        .unwrap();
        send.send(Message::End).unwrap();
        assert_eq!(
            submission.next().await.unwrap().unwrap().get("type"),
            Some(&"turn_completed".into())
        );
        let mut ending = Box::pin(submission.next());
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(ending.as_mut().poll(cx).is_pending())).await
        );
        acknowledged
            .await
            .expect("consumer-finally acknowledges the real end before result settlement");
        tokio::time::advance(closing_budget + Duration::from_millis(1)).await;
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(ending.as_mut().poll(cx).is_pending())).await,
            "ordinary result must not inherit the native publication closing deadline"
        );
        settle.send(Ok(())).unwrap();
        assert!(ending.await.unwrap().is_none());
    }
}
