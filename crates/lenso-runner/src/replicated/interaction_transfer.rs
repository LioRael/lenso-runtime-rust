use std::{
    any::Any,
    cell::Cell,
    collections::BTreeMap,
    fmt,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use futures::future::LocalBoxFuture;
use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::{
    CancellationToken, EventAdmission, EventCapability, InvocationContext, NativeEventEndpoint,
    NativeStreamEndpoint, NativeStreamItem, NativeStreamSession, RuntimeFailure, StreamCapability,
    StreamEvent,
};

use super::transfer::{
    TransferredCancellation, TransferredCancellationGuard, TransferredInvocationContext,
};
use super::{LaneRoute, LaneTask};

type ErasedStreamOpen = Result<Result<Box<dyn NativeStreamSession>, Box<dyn Any>>, RuntimeFailure>;
type ErasedStreamOpenFuture = LocalBoxFuture<'static, ErasedStreamOpen>;

trait StreamTransferFactory: fmt::Debug + Send + Sync {
    fn endpoint(
        &self,
        provider_instance: String,
        provider_lane: LaneRoute,
        epoch: Instant,
    ) -> Rc<dyn NativeStreamEndpoint>;
}

trait EventTransferFactory: fmt::Debug + Send + Sync {
    fn endpoint(
        &self,
        provider_instance: String,
        provider_lane: LaneRoute,
    ) -> Rc<dyn NativeEventEndpoint>;
}

struct TypedStreamTransferFactory<C: StreamCapability> {
    operations: &'static [&'static str],
    next_session_id: Arc<AtomicU64>,
    capability: PhantomData<fn() -> C>,
}

impl<C: StreamCapability> fmt::Debug for TypedStreamTransferFactory<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypedStreamTransferFactory")
            .field("capability", &C::ID)
            .finish_non_exhaustive()
    }
}

impl<C> StreamTransferFactory for TypedStreamTransferFactory<C>
where
    C: StreamCapability,
    C::OpenRequest: Send,
    C::Message: Send,
    C::DomainError: Send,
{
    fn endpoint(
        &self,
        provider_instance: String,
        provider_lane: LaneRoute,
        epoch: Instant,
    ) -> Rc<dyn NativeStreamEndpoint> {
        Rc::new(CrossLaneStreamEndpoint::<C> {
            operations: self.operations,
            provider_instance,
            provider_lane,
            next_session_id: Arc::clone(&self.next_session_id),
            epoch,
            capability: PhantomData,
        })
    }
}

struct TypedEventTransferFactory<C: EventCapability> {
    operations: &'static [&'static str],
    capability: PhantomData<fn() -> C>,
}

impl<C: EventCapability> fmt::Debug for TypedEventTransferFactory<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypedEventTransferFactory")
            .field("capability", &C::ID)
            .finish_non_exhaustive()
    }
}

impl<C> EventTransferFactory for TypedEventTransferFactory<C>
where
    C: EventCapability,
    C::Event: Send,
{
    fn endpoint(
        &self,
        provider_instance: String,
        provider_lane: LaneRoute,
    ) -> Rc<dyn NativeEventEndpoint> {
        Rc::new(CrossLaneEventEndpoint::<C> {
            operations: self.operations,
            provider_instance,
            provider_lane,
            capability: PhantomData,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct CrossLaneInteractionCatalog {
    streams: BTreeMap<&'static str, Arc<dyn StreamTransferFactory>>,
    events: BTreeMap<&'static str, Arc<dyn EventTransferFactory>>,
}

impl CrossLaneInteractionCatalog {
    pub(super) fn with_stream<C>(mut self, operations: &'static [&'static str]) -> Self
    where
        C: StreamCapability,
        C::OpenRequest: Send,
        C::Message: Send,
        C::DomainError: Send,
    {
        self.streams.insert(
            C::ID,
            Arc::new(TypedStreamTransferFactory::<C> {
                operations,
                next_session_id: Arc::new(AtomicU64::new(1)),
                capability: PhantomData,
            }),
        );
        self
    }

    pub(super) fn with_event<C>(mut self, operations: &'static [&'static str]) -> Self
    where
        C: EventCapability,
        C::Event: Send,
    {
        self.events.insert(
            C::ID,
            Arc::new(TypedEventTransferFactory::<C> {
                operations,
                capability: PhantomData,
            }),
        );
        self
    }

    pub(super) fn validate_plan(
        &self,
        plan: &ResolvedAppPlan,
    ) -> Result<(), super::ReplicatedRunnerError> {
        for binding in plan.capability_bindings() {
            let consumer = plan
                .plugin_instance(binding.consumer_instance())
                .expect("validated binding consumer should exist");
            let provider = plan
                .plugin_instance(binding.provider_instance())
                .expect("validated binding provider should exist");
            if consumer.execution_lane() == provider.execution_lane() {
                continue;
            }
            let endpoint = provider
                .provided_capabilities()
                .iter()
                .find(|endpoint| endpoint.capability_id() == binding.capability_id())
                .expect("validated provider endpoint should exist");
            if !endpoint.stream_operations().is_empty()
                && !self.streams.contains_key(binding.capability_id())
            {
                return Err(
                    super::ReplicatedRunnerError::MissingCrossLaneStreamTransfer {
                        capability: binding.capability_id().to_owned(),
                    },
                );
            }
            if !endpoint.event_operations().is_empty()
                && !self.events.contains_key(binding.capability_id())
            {
                return Err(
                    super::ReplicatedRunnerError::MissingCrossLaneEventTransfer {
                        capability: binding.capability_id().to_owned(),
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn stream_endpoint(
        &self,
        capability_id: &str,
        provider_instance: String,
        provider_lane: LaneRoute,
        epoch: Instant,
    ) -> Option<Rc<dyn NativeStreamEndpoint>> {
        self.streams
            .get(capability_id)
            .map(|factory| factory.endpoint(provider_instance, provider_lane, epoch))
    }

    pub(super) fn event_endpoint(
        &self,
        capability_id: &str,
        provider_instance: String,
        provider_lane: LaneRoute,
    ) -> Option<Rc<dyn NativeEventEndpoint>> {
        self.events
            .get(capability_id)
            .map(|factory| factory.endpoint(provider_instance, provider_lane))
    }
}

struct CrossLaneStreamEndpoint<C: StreamCapability> {
    operations: &'static [&'static str],
    provider_instance: String,
    provider_lane: LaneRoute,
    next_session_id: Arc<AtomicU64>,
    epoch: Instant,
    capability: PhantomData<fn() -> C>,
}

impl<C: StreamCapability> fmt::Debug for CrossLaneStreamEndpoint<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrossLaneStreamEndpoint")
            .field("capability", &C::ID)
            .field("provider_instance", &self.provider_instance)
            .finish_non_exhaustive()
    }
}

impl<C> NativeStreamEndpoint for CrossLaneStreamEndpoint<C>
where
    C: StreamCapability,
    C::OpenRequest: Send,
    C::Message: Send,
    C::DomainError: Send,
{
    fn capability_id(&self) -> &'static str {
        C::ID
    }

    fn descriptor_version(&self) -> &'static str {
        C::DESCRIPTOR_VERSION
    }

    fn operations(&self) -> &'static [&'static str] {
        self.operations
    }

    fn open(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        context: InvocationContext,
    ) -> LocalBoxFuture<
        'static,
        Result<Result<Box<dyn NativeStreamSession>, Box<dyn Any>>, RuntimeFailure>,
    > {
        let Some(operation) = self
            .operations
            .iter()
            .copied()
            .find(|candidate| *candidate == operation)
        else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::UnknownOperation {
                    capability: C::ID,
                    operation: operation.to_owned(),
                },
            )));
        };
        let Ok(request) = request.downcast::<C::OpenRequest>() else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        };
        open_cross_lane::<C>(
            self.provider_lane.clone(),
            self.provider_instance.clone(),
            Arc::clone(&self.next_session_id),
            self.epoch,
            operation,
            *request,
            context,
        )
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn open_cross_lane<C>(
    provider_lane: LaneRoute,
    provider_instance: String,
    next_session_id: Arc<AtomicU64>,
    epoch: Instant,
    operation: &'static str,
    request: C::OpenRequest,
    context: InvocationContext,
) -> ErasedStreamOpenFuture
where
    C: StreamCapability,
    C::OpenRequest: Send,
    C::Message: Send,
    C::DomainError: Send,
{
    Box::pin(async move {
        let provider_lane = provider_lane
            .upgrade()
            .ok_or_else(|| lane_unavailable(C::ID))?;
        let caller_instance = context
            .caller_instance()
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "cross-lane stream open of `{}` has no planned caller",
                    C::ID
                ),
            })?
            .to_owned();
        let cancellation = context.cancellation();
        let deadline = context.deadline();
        let request_id = context.request_id();
        let session_id = next_session_id.fetch_add(1, Ordering::Relaxed);
        let (transferred, cancellation_signal) = TransferredInvocationContext::capture(&context);
        let mut cancellation_guard = TransferredCancellationGuard::new(cancellation_signal);
        let provider_cancellation = cancellation_guard.cancellation();
        let (completed, completion) = futures::channel::oneshot::channel();
        let command: LaneTask = Box::new(move |lane| {
            tokio::task::spawn_local(async move {
                let local_cancellation = CancellationToken::new();
                let (context, transferred_cancellation) =
                    transferred.restore(local_cancellation.clone());
                if transferred_cancellation.is_cancelled() {
                    local_cancellation.cancel();
                }
                let handle = match lane.stream_handle::<C>(&caller_instance, &provider_instance) {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _ = completed.send(Err(error));
                        return;
                    }
                };
                let opening = handle.open_with_context(operation, context, request);
                tokio::pin!(opening);
                let result = if local_cancellation.is_cancelled() {
                    opening.await
                } else {
                    tokio::select! {
                        result = &mut opening => result,
                        () = transferred_cancellation.cancelled() => {
                            local_cancellation.cancel();
                            opening.await
                        }
                    }
                };
                let result = match result {
                    Ok(Ok(stream)) if !transferred_cancellation.is_cancelled() => {
                        lane.insert_stream::<C>(session_id, stream);
                        Ok(Ok(()))
                    }
                    Ok(Ok(stream)) => {
                        stream.cancel();
                        Err(RuntimeFailure::Cancelled { request_id })
                    }
                    Ok(Err(error)) => Ok(Err(error)),
                    Err(error) => Err(error),
                };
                let _ = completed.send(result);
            });
        });

        let send = provider_lane.send(command);
        tokio::pin!(send);
        let cancelled = cancellation.cancelled();
        tokio::pin!(cancelled);
        if let Some(deadline) = deadline {
            let sleep = tokio::time::sleep_until((epoch + deadline).into());
            tokio::pin!(sleep);
            tokio::select! {
                result = &mut send => result.map_err(|_| lane_unavailable(C::ID))?,
                () = &mut cancelled => {
                    cancellation_guard.cancel();
                    return Err(RuntimeFailure::Cancelled { request_id });
                }
                () = &mut sleep => {
                    cancellation_guard.cancel();
                    return Err(RuntimeFailure::DeadlineExceeded { request_id });
                }
            }
            tokio::select! {
                biased;
                result = completion => finish_stream_open::<C>(
                    result.map_err(|_| lane_unavailable(C::ID))?,
                    provider_lane.downgrade(),
                    session_id,
                    Arc::clone(&provider_cancellation),
                    &mut cancellation_guard,
                ),
                () = &mut cancelled => {
                    cancellation_guard.cancel();
                    Err(RuntimeFailure::Cancelled { request_id })
                }
                () = &mut sleep => {
                    cancellation_guard.cancel();
                    Err(RuntimeFailure::DeadlineExceeded { request_id })
                }
            }
        } else {
            tokio::select! {
                result = &mut send => result.map_err(|_| lane_unavailable(C::ID))?,
                () = &mut cancelled => {
                    cancellation_guard.cancel();
                    return Err(RuntimeFailure::Cancelled { request_id });
                }
            }
            tokio::select! {
                biased;
                result = completion => finish_stream_open::<C>(
                    result.map_err(|_| lane_unavailable(C::ID))?,
                    provider_lane.downgrade(),
                    session_id,
                    provider_cancellation,
                    &mut cancellation_guard,
                ),
                () = &mut cancelled => {
                    cancellation_guard.cancel();
                    Err(RuntimeFailure::Cancelled { request_id })
                }
            }
        }
    })
}

fn finish_stream_open<C>(
    result: Result<Result<(), C::DomainError>, RuntimeFailure>,
    provider_lane: LaneRoute,
    session_id: u64,
    cancellation: Arc<TransferredCancellation>,
    cancellation_guard: &mut TransferredCancellationGuard,
) -> ErasedStreamOpen
where
    C: StreamCapability,
    C::OpenRequest: Send,
    C::Message: Send,
    C::DomainError: Send,
{
    match result? {
        Ok(()) => {
            cancellation_guard.disarm();
            Ok(Ok(Box::new(CrossLaneStreamSession::<C> {
                provider_lane,
                session_id,
                cancellation,
                retired: Cell::new(false),
                capability: PhantomData,
            })))
        }
        Err(error) => Ok(Err(Box::new(error))),
    }
}

enum StreamAction<C: StreamCapability> {
    Send(C::Message),
    Receive,
    CloseSend,
    Cancel,
}

enum StreamActionResult<C: StreamCapability> {
    Unit,
    Event(StreamEvent<C::Message, C::DomainError>),
}

fn dispatch_stream_action<C>(
    provider_lane: LaneRoute,
    session_id: u64,
    action: StreamAction<C>,
) -> LocalBoxFuture<'static, Result<StreamActionResult<C>, RuntimeFailure>>
where
    C: StreamCapability,
    C::OpenRequest: Send,
    C::Message: Send,
    C::DomainError: Send,
{
    Box::pin(async move {
        let provider_lane = provider_lane
            .upgrade()
            .ok_or_else(|| lane_unavailable(C::ID))?;
        let (completed, completion) = futures::channel::oneshot::channel();
        let command: LaneTask = Box::new(move |lane| {
            tokio::task::spawn_local(async move {
                let stream = match lane.stream::<C>(session_id) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = completed.send(Err(error));
                        return;
                    }
                };
                let result = match action {
                    StreamAction::Send(message) => stream
                        .send(message)
                        .await
                        .map(|()| StreamActionResult::Unit),
                    StreamAction::Receive => stream.receive().await.map(StreamActionResult::Event),
                    StreamAction::CloseSend => {
                        stream.close_send().await.map(|()| StreamActionResult::Unit)
                    }
                    StreamAction::Cancel => {
                        stream.cancel();
                        lane.remove_stream::<C>(session_id);
                        Ok(StreamActionResult::Unit)
                    }
                };
                let should_retire = matches!(
                    &result,
                    Ok(StreamActionResult::Event(StreamEvent::Terminal(_)))
                ) || matches!(&result, Err(error) if !matches!(error, RuntimeFailure::ResourceExhausted { .. }));
                if should_retire {
                    lane.remove_stream::<C>(session_id);
                }
                let _ = completed.send(result);
            });
        });
        provider_lane
            .send(command)
            .await
            .map_err(|_| lane_unavailable(C::ID))?;
        completion.await.map_err(|_| lane_unavailable(C::ID))?
    })
}

struct CrossLaneStreamSession<C: StreamCapability> {
    provider_lane: LaneRoute,
    session_id: u64,
    cancellation: Arc<TransferredCancellation>,
    retired: Cell<bool>,
    capability: PhantomData<fn() -> C>,
}

impl<C: StreamCapability> fmt::Debug for CrossLaneStreamSession<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrossLaneStreamSession")
            .field("capability", &C::ID)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl<C> NativeStreamSession for CrossLaneStreamSession<C>
where
    C: StreamCapability,
    C::OpenRequest: Send,
    C::Message: Send,
    C::DomainError: Send,
{
    fn send(&self, message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let Ok(message) = message.downcast::<C::Message>() else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        };
        let action = dispatch_stream_action::<C>(
            self.provider_lane.clone(),
            self.session_id,
            StreamAction::Send(*message),
        );
        Box::pin(async move {
            match action.await? {
                StreamActionResult::Unit => Ok(()),
                StreamActionResult::Event(_) => {
                    Err(RuntimeFailure::ProtocolViolation { capability: C::ID })
                }
            }
        })
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let action = dispatch_stream_action::<C>(
            self.provider_lane.clone(),
            self.session_id,
            StreamAction::Receive,
        );
        Box::pin(async move {
            match action.await? {
                StreamActionResult::Event(StreamEvent::Message(message)) => {
                    Ok(NativeStreamItem::Message(Box::new(message)))
                }
                StreamActionResult::Event(StreamEvent::PeerHalfClosed) => {
                    Ok(NativeStreamItem::PeerHalfClosed)
                }
                StreamActionResult::Event(StreamEvent::Terminal(Ok(()))) => {
                    Ok(NativeStreamItem::Terminal(Ok(())))
                }
                StreamActionResult::Event(StreamEvent::Terminal(Err(error))) => {
                    Ok(NativeStreamItem::Terminal(Err(Box::new(error))))
                }
                StreamActionResult::Unit => {
                    Err(RuntimeFailure::ProtocolViolation { capability: C::ID })
                }
            }
        })
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let action = dispatch_stream_action::<C>(
            self.provider_lane.clone(),
            self.session_id,
            StreamAction::CloseSend,
        );
        Box::pin(async move {
            match action.await? {
                StreamActionResult::Unit => Ok(()),
                StreamActionResult::Event(_) => {
                    Err(RuntimeFailure::ProtocolViolation { capability: C::ID })
                }
            }
        })
    }

    fn cancel(&self) {
        if self.retired.replace(true) {
            return;
        }
        self.cancellation.cancel();
        let cleanup = dispatch_stream_action::<C>(
            self.provider_lane.clone(),
            self.session_id,
            StreamAction::Cancel,
        );
        tokio::task::spawn_local(async move {
            let _ = cleanup.await;
        });
    }
}

impl<C: StreamCapability> Drop for CrossLaneStreamSession<C> {
    fn drop(&mut self) {
        if !self.retired.get() {
            self.cancellation.cancel();
        }
    }
}

struct CrossLaneEventEndpoint<C: EventCapability> {
    operations: &'static [&'static str],
    provider_instance: String,
    provider_lane: LaneRoute,
    capability: PhantomData<fn() -> C>,
}

impl<C: EventCapability> fmt::Debug for CrossLaneEventEndpoint<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrossLaneEventEndpoint")
            .field("capability", &C::ID)
            .field("provider_instance", &self.provider_instance)
            .finish_non_exhaustive()
    }
}

impl<C> NativeEventEndpoint for CrossLaneEventEndpoint<C>
where
    C: EventCapability,
    C::Event: Send,
{
    fn capability_id(&self) -> &'static str {
        C::ID
    }

    fn descriptor_version(&self) -> &'static str {
        C::DESCRIPTOR_VERSION
    }

    fn operations(&self) -> &'static [&'static str] {
        self.operations
    }

    fn owns_event_admission(&self) -> bool {
        true
    }

    fn publish(
        &self,
        operation: &str,
        event: Box<dyn Any>,
        context: InvocationContext,
    ) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let Some(operation) = self
            .operations
            .iter()
            .copied()
            .find(|candidate| *candidate == operation)
        else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::UnknownOperation {
                    capability: C::ID,
                    operation: operation.to_owned(),
                },
            )));
        };
        let Ok(event) = event.downcast::<C::Event>() else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        };
        publish_event_cross_lane::<C>(
            self.provider_lane.clone(),
            self.provider_instance.clone(),
            operation,
            *event,
            context,
        )
    }
}

fn publish_event_cross_lane<C>(
    provider_lane: LaneRoute,
    provider_instance: String,
    operation: &'static str,
    event: C::Event,
    context: InvocationContext,
) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>>
where
    C: EventCapability,
    C::Event: Send,
{
    Box::pin(async move {
        let provider_lane = provider_lane
            .upgrade()
            .ok_or_else(|| lane_unavailable(C::ID))?;
        let caller_instance = context
            .caller_instance()
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "cross-lane Event publish of `{}` has no planned caller",
                    C::ID
                ),
            })?
            .to_owned();
        let (transferred, cancellation_signal) = TransferredInvocationContext::capture(&context);
        let mut cancellation_guard = TransferredCancellationGuard::new(cancellation_signal);
        let (completed, completion) = futures::channel::oneshot::channel();
        let command: LaneTask = Box::new(move |lane| {
            tokio::task::spawn_local(async move {
                let local_cancellation = CancellationToken::new();
                let (context, transferred_cancellation) = transferred.restore(local_cancellation);
                if transferred_cancellation.is_cancelled() {
                    let _ = completed.send(Err(RuntimeFailure::Cancelled {
                        request_id: context.request_id(),
                    }));
                    return;
                }
                let handle = match lane.event_handle::<C>(&caller_instance, &provider_instance) {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _ = completed.send(Err(error));
                        return;
                    }
                };
                let results = handle.publish_with_context(operation, context, event).await;
                let result = match results.as_slice() {
                    [result] => match result.admission() {
                        EventAdmission::Accepted => Ok(()),
                        EventAdmission::Exhausted => Err(RuntimeFailure::ResourceExhausted {
                            capability: C::ID,
                            operation: operation.to_owned(),
                        }),
                        EventAdmission::Unavailable => {
                            Err(RuntimeFailure::Unavailable { capability: C::ID })
                        }
                    },
                    results => Err(RuntimeFailure::Internal {
                        detail: format!(
                            "cross-lane Event binding for `{}` resolved {} provider endpoints",
                            C::ID,
                            results.len()
                        ),
                    }),
                };
                let _ = completed.send(result);
            });
        });
        provider_lane
            .send(command)
            .await
            .map_err(|_| lane_unavailable(C::ID))?;
        let result = completion.await.map_err(|_| lane_unavailable(C::ID))?;
        cancellation_guard.disarm();
        result
    })
}

fn lane_unavailable(capability: &'static str) -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: format!("provider lane for `{capability}` is unavailable"),
    }
}
