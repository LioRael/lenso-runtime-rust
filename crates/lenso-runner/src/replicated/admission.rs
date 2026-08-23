use std::time::Duration;

use futures::{channel::oneshot, future::pending};
use lenso_app_plan::ExecutionLaneId;
use lenso_kernel::{RequestId, RuntimeFailure};

use super::{LaneCancellationToken, LaneSender, LaneTask};

// Kernel Request IDs start at one. Zero truthfully denotes a failure before the
// provider lane could allocate an invocation identity.
const PRE_ADMISSION_REQUEST_ID: RequestId = 0;

pub(super) async fn dispatch_controlled<T>(
    lane: &ExecutionLaneId,
    commands: &LaneSender,
    task: LaneTask,
    mut started: oneshot::Receiver<()>,
    mut completion: oneshot::Receiver<T>,
    timeout: Option<Duration>,
    cancellation: Option<LaneCancellationToken>,
) -> Result<T, RuntimeFailure> {
    let cancelled = wait_for_cancellation(cancellation);
    let deadline = wait_for_deadline(timeout);
    let dispatch = commands.send(task);
    tokio::pin!(cancelled, deadline, dispatch);

    tokio::select! {
        biased;
        () = &mut cancelled => return Err(cancelled_before_admission()),
        () = &mut deadline => return Err(deadline_before_admission()),
        result = &mut dispatch => result.map_err(|_| lane_unavailable(lane))?,
    }

    tokio::select! {
        biased;
        result = &mut completion => return result.map_err(|_| invocation_dropped(lane)),
        result = &mut started => {
            if result.is_err() {
                return completion.await.map_err(|_| invocation_dropped(lane));
            }
        }
        () = &mut cancelled => return Err(cancelled_before_admission()),
        () = &mut deadline => return Err(deadline_before_admission()),
    }

    completion.await.map_err(|_| invocation_dropped(lane))
}

async fn wait_for_cancellation(cancellation: Option<LaneCancellationToken>) {
    if let Some(cancellation) = cancellation {
        cancellation.cancelled().await;
    } else {
        pending::<()>().await;
    }
}

async fn wait_for_deadline(timeout: Option<Duration>) {
    if let Some(timeout) = timeout {
        tokio::time::sleep(timeout).await;
    } else {
        pending::<()>().await;
    }
}

const fn cancelled_before_admission() -> RuntimeFailure {
    RuntimeFailure::Cancelled {
        request_id: PRE_ADMISSION_REQUEST_ID,
    }
}

const fn deadline_before_admission() -> RuntimeFailure {
    RuntimeFailure::DeadlineExceeded {
        request_id: PRE_ADMISSION_REQUEST_ID,
    }
}

fn lane_unavailable(lane: &ExecutionLaneId) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: format!("Execution Lane `{lane}` is unavailable"),
    }
}

fn invocation_dropped(lane: &ExecutionLaneId) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: format!("Execution Lane `{lane}` dropped an invocation"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::channel::oneshot;
    use lenso_app_plan::ExecutionLaneId;
    use lenso_kernel::RuntimeFailure;
    use tokio::sync::{mpsc, oneshot as tokio_oneshot};

    use super::{
        super::{LaneCancellationToken, LaneSender, LaneTask},
        dispatch_controlled,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn deadline_covers_a_saturated_lane_command_queue() {
        let (commands, mut queued): (LaneSender, mpsc::Receiver<LaneTask>) = mpsc::channel(1);
        commands
            .send(Box::new(|_| {}))
            .await
            .expect("the fixture command should fill the queue");
        let (_started, start) = oneshot::channel();
        let (_completed, completion) = oneshot::channel::<()>();

        let failure = dispatch_controlled(
            &ExecutionLaneId::new("workers"),
            &commands,
            Box::new(|_| {}),
            start,
            completion,
            Some(Duration::from_millis(1)),
            None,
        )
        .await
        .expect_err("the deadline should expire before lane admission");

        assert_eq!(failure, RuntimeFailure::DeadlineExceeded { request_id: 0 });
        let _ = queued.recv().await;
        assert!(matches!(
            queued.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_covers_a_saturated_lane_command_queue() {
        let (commands, mut queued): (LaneSender, mpsc::Receiver<LaneTask>) = mpsc::channel(1);
        commands
            .send(Box::new(|_| {}))
            .await
            .expect("the fixture command should fill the queue");
        let (_started, start) = oneshot::channel();
        let (_completed, completion) = oneshot::channel::<()>();
        let cancellation = LaneCancellationToken::new();
        let cancel = cancellation.clone();
        let (cancelled, observe_cancelled) = tokio_oneshot::channel();

        let (failure, ()) = tokio::join!(
            async {
                dispatch_controlled(
                    &ExecutionLaneId::new("workers"),
                    &commands,
                    Box::new(|_| {}),
                    start,
                    completion,
                    None,
                    Some(cancellation),
                )
                .await
                .expect_err("cancellation should stop lane admission")
            },
            async move {
                tokio::task::yield_now().await;
                cancel.cancel();
                let _ = cancelled.send(());
            }
        );
        observe_cancelled
            .await
            .expect("the fixture should issue cancellation");

        assert_eq!(failure, RuntimeFailure::Cancelled { request_id: 0 });
        let _ = queued.recv().await;
        assert!(matches!(
            queued.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}
