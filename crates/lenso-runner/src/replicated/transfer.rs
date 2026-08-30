use std::{
    any::Any,
    collections::BTreeMap,
    fmt,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use super::{LaneRoute, LaneTask};
use futures::{future::LocalBoxFuture, task::AtomicWaker};
use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::{
    CancellationToken, InvocationContext, NativeRequestEndpoint, NativeRequestFuture,
    RequestCapability, RuntimeFailure, TypedNativeRequestEndpoint,
};

trait RequestTransferFactory: fmt::Debug + Send + Sync {
    fn endpoint(&self, provider_lane: LaneRoute, epoch: Instant) -> Rc<dyn NativeRequestEndpoint>;
}

struct TypedRequestTransferFactory<C: RequestCapability> {
    operations: &'static [&'static str],
    capability: PhantomData<fn() -> C>,
}

impl<C: RequestCapability> fmt::Debug for TypedRequestTransferFactory<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypedRequestTransferFactory")
            .field("capability", &C::ID)
            .finish_non_exhaustive()
    }
}

impl<C> RequestTransferFactory for TypedRequestTransferFactory<C>
where
    C: RequestCapability,
    C::Request: Send,
    C::Response: Send,
    C::DomainError: Send,
{
    fn endpoint(&self, provider_lane: LaneRoute, epoch: Instant) -> Rc<dyn NativeRequestEndpoint> {
        let typed_provider_lane = provider_lane.clone();
        let operations = self.operations;
        Rc::new(CrossLaneRequestEndpoint::<C> {
            operations: self.operations,
            typed: TypedNativeRequestEndpoint::new(move |operation, request, context| {
                let Some(operation) = operations
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
                invoke_cross_lane::<C>(
                    typed_provider_lane.clone(),
                    epoch,
                    operation,
                    request,
                    context,
                )
            }),
        })
    }
}

/// Native request types registered for zero-serialization cross-lane transfer.
#[derive(Clone, Debug, Default)]
pub struct CrossLaneRequestCatalog {
    factories: BTreeMap<&'static str, Arc<dyn RequestTransferFactory>>,
}

impl CrossLaneRequestCatalog {
    /// Creates an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one generated request Capability whose values are `Send`.
    #[must_use]
    pub fn with_request<C>(mut self, operations: &'static [&'static str]) -> Self
    where
        C: RequestCapability,
        C::Request: Send,
        C::Response: Send,
        C::DomainError: Send,
    {
        self.factories.insert(
            C::ID,
            Arc::new(TypedRequestTransferFactory::<C> {
                operations,
                capability: PhantomData,
            }),
        );
        self
    }

    pub(super) fn contains(&self, capability_id: &str) -> bool {
        self.factories.contains_key(capability_id)
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
            let endpoint = provider
                .provided_capabilities()
                .iter()
                .find(|endpoint| endpoint.capability_id() == binding.capability_id())
                .expect("validated provider endpoint should exist");
            if consumer.execution_lane() != provider.execution_lane()
                && !endpoint.request_operations().is_empty()
                && !self.contains(binding.capability_id())
            {
                return Err(
                    super::ReplicatedRunnerError::MissingCrossLaneRequestTransfer {
                        capability: binding.capability_id().to_owned(),
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn endpoint(
        &self,
        capability_id: &str,
        provider_lane: LaneRoute,
        epoch: Instant,
    ) -> Option<Rc<dyn NativeRequestEndpoint>> {
        self.factories
            .get(capability_id)
            .map(|factory| factory.endpoint(provider_lane, epoch))
    }
}

struct CrossLaneRequestEndpoint<C: RequestCapability> {
    operations: &'static [&'static str],
    typed: TypedNativeRequestEndpoint<C>,
}

impl<C: RequestCapability> fmt::Debug for CrossLaneRequestEndpoint<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrossLaneRequestEndpoint")
            .field("capability", &C::ID)
            .finish_non_exhaustive()
    }
}

impl<C> NativeRequestEndpoint for CrossLaneRequestEndpoint<C>
where
    C: RequestCapability,
    C::Request: Send,
    C::Response: Send,
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

    fn typed_endpoint(&self) -> Option<&dyn Any> {
        Some(&self.typed)
    }

    fn invoke(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        context: InvocationContext,
    ) -> LocalBoxFuture<'static, Result<Result<Box<dyn Any>, Box<dyn Any>>, RuntimeFailure>> {
        let Ok(request) = request.downcast::<C::Request>() else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        };
        let invocation = self.typed.invoke(operation, *request, context);
        Box::pin(async move {
            let result = invocation.await?;
            Ok(result
                .map(|response| Box::new(response) as Box<dyn Any>)
                .map_err(|error| Box::new(error) as Box<dyn Any>))
        })
    }
}

#[allow(clippy::too_many_lines)]
fn invoke_cross_lane<C>(
    provider_lane: LaneRoute,
    epoch: Instant,
    operation: &'static str,
    request: C::Request,
    context: InvocationContext,
) -> NativeRequestFuture<C>
where
    C: RequestCapability,
    C::Request: Send,
    C::Response: Send,
    C::DomainError: Send,
{
    Box::pin(async move {
        let provider_lane = provider_lane.upgrade().ok_or_else(lane_unavailable::<C>)?;
        let caller_instance = context
            .caller_instance()
            .ok_or_else(|| RuntimeFailure::InvalidResolvedPlan {
                detail: format!("cross-lane invocation of `{}` has no planned caller", C::ID),
            })?
            .to_owned();
        let cancellation = context.cancellation();
        let deadline = context.deadline();
        let request_id = context.request_id();
        let (transferred, cancellation_signal) = TransferredInvocationContext::capture(&context);
        let mut cancellation_guard = TransferredCancellationGuard::new(cancellation_signal);
        let (completed, completion) = futures::channel::oneshot::channel();
        let command: LaneTask = Box::new(move |lane| {
            tokio::task::spawn_local(async move {
                let local_cancellation = CancellationToken::new();
                let (context, transferred_cancellation) =
                    transferred.restore(local_cancellation.clone());
                if transferred_cancellation.is_cancelled() {
                    local_cancellation.cancel();
                }
                let handle = match lane.request_handle::<C>(&caller_instance) {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _ = completed.send(Err(error));
                        return;
                    }
                };
                let invocation = handle.invoke_with_context(operation, context, request);
                tokio::pin!(invocation);
                let result = if local_cancellation.is_cancelled() {
                    invocation.await
                } else {
                    tokio::select! {
                        result = &mut invocation => result,
                        () = transferred_cancellation.cancelled() => {
                            local_cancellation.cancel();
                            invocation.await
                        }
                    }
                };
                let _ = completed.send(result);
            });
        });

        let cancelled = cancellation.cancelled();
        tokio::pin!(cancelled);
        let result = if let Some(deadline) = deadline {
            let sleep = tokio::time::sleep_until((epoch + deadline).into());
            tokio::pin!(sleep);
            if let Err(error) = provider_lane.try_send(command) {
                let command = error.into_inner();
                let send = provider_lane.send(command);
                tokio::pin!(send);
                tokio::select! {
                    result = &mut send => result.map_err(|_| lane_unavailable::<C>())?,
                    () = &mut cancelled => {
                        cancellation_guard.cancel();
                        return Err(RuntimeFailure::Cancelled { request_id });
                    }
                    () = &mut sleep => {
                        cancellation_guard.cancel();
                        return Err(RuntimeFailure::DeadlineExceeded { request_id });
                    }
                }
            }
            tokio::select! {
                biased;
                result = completion => result.map_err(|_| lane_unavailable::<C>())?,
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
            if let Err(error) = provider_lane.try_send(command) {
                let send = provider_lane.send(error.into_inner());
                tokio::pin!(send);
                tokio::select! {
                    result = &mut send => result.map_err(|_| lane_unavailable::<C>())?,
                    () = &mut cancelled => {
                        cancellation_guard.cancel();
                        return Err(RuntimeFailure::Cancelled { request_id });
                    }
                }
            }
            tokio::select! {
                biased;
                result = completion => result.map_err(|_| lane_unavailable::<C>())?,
                () = &mut cancelled => {
                    cancellation_guard.cancel();
                    Err(RuntimeFailure::Cancelled { request_id })
                }
            }
        };
        cancellation_guard.disarm();
        result
    })
}

fn lane_unavailable<C: RequestCapability>() -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: format!("provider lane for `{}` is unavailable", C::ID),
    }
}

#[derive(Debug)]
pub(super) struct TransferredInvocationContext {
    request_id: u64,
    deadline: Option<std::time::Duration>,
    cancellation: Arc<TransferredCancellation>,
    extensions: Vec<lenso_kernel::InvocationExtension>,
    sealed_extensions: Vec<lenso_kernel::SealedInvocationExtension>,
}

impl TransferredInvocationContext {
    pub(super) fn capture(context: &InvocationContext) -> (Self, Arc<TransferredCancellation>) {
        let cancellation = Arc::new(TransferredCancellation::new(context.is_cancelled()));
        (
            Self {
                request_id: context.request_id(),
                deadline: context.deadline(),
                cancellation: Arc::clone(&cancellation),
                extensions: context.extensions().cloned().collect(),
                sealed_extensions: context.sealed_extensions().cloned().collect(),
            },
            cancellation,
        )
    }

    pub(super) fn restore(
        self,
        cancellation: CancellationToken,
    ) -> (InvocationContext, Arc<TransferredCancellation>) {
        let mut context = InvocationContext::new(self.request_id, self.deadline, cancellation);
        for extension in self.extensions {
            context = context
                .with_extension(extension.key(), extension.value().to_vec())
                .expect("captured ordinary Invocation Context extension remains valid");
        }
        for extension in self.sealed_extensions {
            context = context
                .with_sealed_extension(extension)
                .expect("captured sealed Invocation Context extension remains valid");
        }
        (context, self.cancellation)
    }
}

#[derive(Debug)]
pub(super) struct TransferredCancellation {
    // A transferred invocation has exactly one provider-side waiter.
    state: AtomicU8,
    waker: AtomicWaker,
}

const TRANSFER_PENDING: u8 = 0;
const TRANSFER_CANCELLED: u8 = 1;
const TRANSFER_ACCEPTED: u8 = 2;

/// Ensures dropping a caller-side transfer future wakes its provider-side cancellation waiter.
#[derive(Debug)]
pub(super) struct TransferredCancellationGuard {
    cancellation: Arc<TransferredCancellation>,
    armed: bool,
}

impl TransferredCancellationGuard {
    pub(super) fn new(cancellation: Arc<TransferredCancellation>) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    pub(super) fn cancellation(&self) -> Arc<TransferredCancellation> {
        Arc::clone(&self.cancellation)
    }

    pub(super) fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TransferredCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

impl TransferredCancellation {
    fn new(cancelled: bool) -> Self {
        Self {
            state: AtomicU8::new(if cancelled {
                TRANSFER_CANCELLED
            } else {
                TRANSFER_PENDING
            }),
            waker: AtomicWaker::new(),
        }
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == TRANSFER_CANCELLED
    }

    pub(super) fn cancel(&self) {
        if self.state.swap(TRANSFER_CANCELLED, Ordering::AcqRel) != TRANSFER_CANCELLED {
            self.waker.wake();
        }
    }

    pub(super) fn accept(&self) {
        if self
            .state
            .compare_exchange(
                TRANSFER_PENDING,
                TRANSFER_ACCEPTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.waker.wake();
        }
    }

    pub(super) fn cancelled(&self) -> TransferredCancellationFuture<'_> {
        TransferredCancellationFuture { cancellation: self }
    }

    pub(super) fn settled(&self) -> TransferredSettlementFuture<'_> {
        TransferredSettlementFuture { cancellation: self }
    }
}

pub(super) struct TransferredCancellationFuture<'a> {
    cancellation: &'a TransferredCancellation,
}

pub(super) struct TransferredSettlementFuture<'a> {
    cancellation: &'a TransferredCancellation,
}

impl Future for TransferredSettlementFuture<'_> {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let state = self.cancellation.state.load(Ordering::Acquire);
        if state != TRANSFER_PENDING {
            return Poll::Ready(state == TRANSFER_CANCELLED);
        }
        self.cancellation.waker.register(context.waker());
        let state = self.cancellation.state.load(Ordering::Acquire);
        if state == TRANSFER_PENDING {
            Poll::Pending
        } else {
            Poll::Ready(state == TRANSFER_CANCELLED)
        }
    }
}

impl Future for TransferredCancellationFuture<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.cancellation.is_cancelled() {
            return Poll::Ready(());
        }
        self.cancellation.waker.register(context.waker());
        if self.cancellation.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::TransferredCancellation;

    #[tokio::test(flavor = "current_thread")]
    async fn transferred_cancellation_observes_an_initial_signal() {
        let cancellation = TransferredCancellation::new(true);

        cancellation.cancelled().await;

        assert!(cancellation.is_cancelled());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transferred_cancellation_wakes_the_provider_waiter() {
        let cancellation = Arc::new(TransferredCancellation::new(false));
        let canceller = Arc::clone(&cancellation);

        tokio::join!(cancellation.cancelled(), async move {
            tokio::task::yield_now().await;
            canceller.cancel();
        });

        assert!(cancellation.is_cancelled());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transferred_acceptance_settles_without_cancellation() {
        let cancellation = Arc::new(TransferredCancellation::new(false));
        let accepter = Arc::clone(&cancellation);

        let cancelled = tokio::join!(cancellation.settled(), async move {
            tokio::task::yield_now().await;
            accepter.accept();
        })
        .0;

        assert!(!cancelled);
        assert!(!cancellation.is_cancelled());
    }
}
