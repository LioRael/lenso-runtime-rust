use std::{
    any::Any, collections::BTreeMap, fmt, marker::PhantomData, rc::Rc, sync::Arc, time::Instant,
};

use futures::future::LocalBoxFuture;
use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::{
    CancellationToken, InvocationContext, NativeRequestEndpoint, RequestCapability, RuntimeFailure,
};
use tokio::sync::watch;

use super::{LaneRoute, LaneTask};

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
        Rc::new(CrossLaneRequestEndpoint::<C> {
            operations: self.operations,
            provider_lane,
            epoch,
            capability: PhantomData,
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
                .module_instance(binding.consumer_instance())
                .expect("validated binding consumer should exist");
            let provider = plan
                .module_instance(binding.provider_instance())
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
    provider_lane: LaneRoute,
    epoch: Instant,
    capability: PhantomData<fn() -> C>,
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
        let provider_lane = self.provider_lane.clone();
        let operation = operation.to_owned();
        let epoch = self.epoch;
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
            let (transferred, cancellation_signal) =
                TransferredInvocationContext::capture(&context);
            let (completed, completion) = futures::channel::oneshot::channel();
            let command: LaneTask = Box::new(move |lane| {
                Box::pin(async move {
                    let local_cancellation = CancellationToken::new();
                    let (context, mut transferred_cancellation) =
                        transferred.restore(local_cancellation.clone());
                    if *transferred_cancellation.borrow() {
                        local_cancellation.cancel();
                    }
                    let handle = match lane.request_handle::<C>(&caller_instance) {
                        Ok(handle) => handle,
                        Err(error) => {
                            let _ = completed.send(Err(error));
                            return;
                        }
                    };
                    let invocation = handle.invoke_with_context(&operation, context, *request);
                    tokio::pin!(invocation);
                    let result = if local_cancellation.is_cancelled() {
                        invocation.await
                    } else {
                        tokio::select! {
                            result = &mut invocation => result,
                            () = wait_for_cancellation(&mut transferred_cancellation) => {
                                local_cancellation.cancel();
                                invocation.await
                            }
                        }
                    };
                    let _ = completed.send(result);
                })
            });

            let send = provider_lane.send(command);
            tokio::pin!(send);
            let cancelled = cancellation.cancelled();
            tokio::pin!(cancelled);
            let result = match deadline {
                Some(deadline) => {
                    let sleep = tokio::time::sleep_until((epoch + deadline).into());
                    tokio::pin!(sleep);
                    tokio::select! {
                        result = &mut send => result.map_err(|_| lane_unavailable::<C>())?,
                        () = &mut cancelled => {
                            cancellation_signal.send_replace(true);
                            return Err(RuntimeFailure::Cancelled { request_id });
                        }
                        () = &mut sleep => {
                            cancellation_signal.send_replace(true);
                            return Err(RuntimeFailure::DeadlineExceeded { request_id });
                        }
                    }
                    tokio::select! {
                        biased;
                        result = completion => result.map_err(|_| lane_unavailable::<C>())?,
                        () = &mut cancelled => {
                            cancellation_signal.send_replace(true);
                            return Err(RuntimeFailure::Cancelled { request_id });
                        }
                        () = &mut sleep => {
                            cancellation_signal.send_replace(true);
                            return Err(RuntimeFailure::DeadlineExceeded { request_id });
                        }
                    }
                }
                None => {
                    tokio::select! {
                        result = &mut send => result.map_err(|_| lane_unavailable::<C>())?,
                        () = &mut cancelled => {
                            cancellation_signal.send_replace(true);
                            return Err(RuntimeFailure::Cancelled { request_id });
                        }
                    }
                    tokio::select! {
                        biased;
                        result = completion => result.map_err(|_| lane_unavailable::<C>())?,
                        () = &mut cancelled => {
                            cancellation_signal.send_replace(true);
                            return Err(RuntimeFailure::Cancelled { request_id });
                        }
                    }
                }
            };
            result.map(|domain| {
                domain
                    .map(|response| Box::new(response) as Box<dyn Any>)
                    .map_err(|error| Box::new(error) as Box<dyn Any>)
            })
        })
    }
}

fn lane_unavailable<C: RequestCapability>() -> RuntimeFailure {
    RuntimeFailure::Internal {
        detail: format!("provider lane for `{}` is unavailable", C::ID),
    }
}

#[derive(Debug)]
struct TransferredInvocationContext {
    request_id: u64,
    deadline: Option<std::time::Duration>,
    cancellation: watch::Receiver<bool>,
    extensions: Vec<lenso_kernel::InvocationExtension>,
    sealed_extensions: Vec<lenso_kernel::SealedInvocationExtension>,
}

impl TransferredInvocationContext {
    fn capture(context: &InvocationContext) -> (Self, watch::Sender<bool>) {
        let (cancellation_signal, cancellation) = watch::channel(context.is_cancelled());
        (
            Self {
                request_id: context.request_id(),
                deadline: context.deadline(),
                cancellation,
                extensions: context.extensions().cloned().collect(),
                sealed_extensions: context.sealed_extensions().cloned().collect(),
            },
            cancellation_signal,
        )
    }

    fn restore(
        self,
        cancellation: CancellationToken,
    ) -> (InvocationContext, watch::Receiver<bool>) {
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

async fn wait_for_cancellation(cancellation: &mut watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow_and_update() {
            return;
        }
        if cancellation.changed().await.is_err() {
            return;
        }
    }
}
