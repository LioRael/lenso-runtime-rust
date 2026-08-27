use std::{any::Any, cell::RefCell, collections::VecDeque, rc::Rc, sync::mpsc, time::Duration};

use futures::future::{LocalBoxFuture, ready};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionLaneId, ExecutionLanePlan, PluginInstancePlan,
};
use lenso_kernel::{
    ActivateContext, CancellationToken, EventAdmission, EventCapability, ExecutionAdapterCatalog,
    InvocationContext, NativeEventEndpoint, NativeStreamEndpoint, NativeStreamItem,
    NativeStreamSession, NoopPluginLifecycle, PluginLifecycle, RuntimeFailure, StreamCapability,
    StreamEvent,
};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_runner::{CrossLaneTransferCatalog, ReplicatedNativeApp, ReplicatedRunnerError};

const STREAM_ID: &str = "fixture.cross-lane-stream@1";
const EVENT_ID: &str = "fixture.cross-lane-event@1";
const VERSION: &str = "1.0.0";
const STREAM_OPERATION: &str = "exchange";
const EVENT_OPERATION: &str = "publish";
const CONSUMER_PACKAGE: &str = "fixture.interaction-consumer";
const STREAM_PROVIDER_PACKAGE: &str = "fixture.stream-provider";
const EVENT_PROVIDER_PACKAGE: &str = "fixture.event-provider";

#[derive(Debug)]
struct TestStream;

impl StreamCapability for TestStream {
    type OpenRequest = String;
    type Message = String;
    type DomainError = &'static str;

    const ID: &'static str = STREAM_ID;
    const DESCRIPTOR_VERSION: &'static str = VERSION;
}

#[derive(Debug)]
struct TestEvent;

impl EventCapability for TestEvent {
    type Event = String;

    const ID: &'static str = EVENT_ID;
    const DESCRIPTOR_VERSION: &'static str = VERSION;
}

#[derive(Debug)]
struct EchoStreamEndpoint;

impl NativeStreamEndpoint for EchoStreamEndpoint {
    fn capability_id(&self) -> &'static str {
        STREAM_ID
    }

    fn descriptor_version(&self) -> &'static str {
        VERSION
    }

    fn operations(&self) -> &'static [&'static str] {
        &[STREAM_OPERATION]
    }

    fn open(
        &self,
        operation: &str,
        request: Box<dyn Any>,
        _context: InvocationContext,
    ) -> LocalBoxFuture<
        'static,
        Result<Result<Box<dyn NativeStreamSession>, Box<dyn Any>>, RuntimeFailure>,
    > {
        if operation != STREAM_OPERATION {
            return Box::pin(ready(Err(RuntimeFailure::UnknownOperation {
                capability: STREAM_ID,
                operation: operation.to_owned(),
            })));
        }
        let Ok(request) = request.downcast::<String>() else {
            return Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
                capability: STREAM_ID,
            })));
        };
        if request.as_str() == "reject" {
            return Box::pin(ready(Ok(Err(Box::new("rejected") as Box<dyn Any>))));
        }
        let slow = request.as_str() == "slow";
        let session: Box<dyn NativeStreamSession> = Box::new(EchoStreamSession {
            prefix: *request,
            pending: Rc::new(RefCell::new(VecDeque::new())),
        });
        if slow {
            return Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(Ok(session))
            });
        }
        Box::pin(ready(Ok(Ok(session))))
    }
}

#[derive(Debug)]
struct EchoStreamSession {
    prefix: String,
    pending: Rc<RefCell<VecDeque<NativeStreamItem>>>,
}

impl NativeStreamSession for EchoStreamSession {
    fn send(&self, message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let Ok(message) = message.downcast::<String>() else {
            return Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
                capability: STREAM_ID,
            })));
        };
        self.pending
            .borrow_mut()
            .push_back(NativeStreamItem::Message(Box::new(format!(
                "{}:{message}",
                self.prefix
            ))));
        Box::pin(ready(Ok(())))
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let item = self
            .pending
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| RuntimeFailure::Internal {
                detail: "test stream has no pending item".to_owned(),
            });
        Box::pin(ready(item))
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let mut pending = self.pending.borrow_mut();
        pending.push_back(NativeStreamItem::PeerHalfClosed);
        pending.push_back(NativeStreamItem::Terminal(Ok(())));
        Box::pin(ready(Ok(())))
    }

    fn cancel(&self) {
        self.pending.borrow_mut().clear();
    }
}

#[derive(Debug)]
struct ReportingEventEndpoint {
    reported: mpsc::Sender<(String, String)>,
    provider: String,
}

impl NativeEventEndpoint for ReportingEventEndpoint {
    fn capability_id(&self) -> &'static str {
        EVENT_ID
    }

    fn descriptor_version(&self) -> &'static str {
        VERSION
    }

    fn operations(&self) -> &'static [&'static str] {
        &[EVENT_OPERATION]
    }

    fn publish(
        &self,
        operation: &str,
        event: Box<dyn Any>,
        _context: InvocationContext,
    ) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        if operation != EVENT_OPERATION {
            return Box::pin(ready(Err(RuntimeFailure::UnknownOperation {
                capability: EVENT_ID,
                operation: operation.to_owned(),
            })));
        }
        let Ok(event) = event.downcast::<String>() else {
            return Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
                capability: EVENT_ID,
            })));
        };
        let _ = self.reported.send((self.provider.clone(), *event));
        Box::pin(ready(Ok(())))
    }
}

#[derive(Debug)]
struct ConsumerFactory {
    reported: mpsc::Sender<ConsumerOutcome>,
}

impl NativePluginFactory for ConsumerFactory {
    fn package_id(&self) -> &'static str {
        CONSUMER_PACKAGE
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::with_lifecycle(
            Vec::new(),
            ConsumerLifecycle {
                reported: self.reported.clone(),
            },
        ))
    }
}

#[derive(Debug)]
struct ConsumerLifecycle {
    reported: mpsc::Sender<ConsumerOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConsumerOutcome {
    message: String,
    protocol_complete: bool,
    rejected: bool,
    cancelled: bool,
    admissions: Vec<EventAdmission>,
}

impl PluginLifecycle for ConsumerLifecycle {
    fn activate(&self, context: ActivateContext) -> lenso_kernel::PluginFuture {
        let stream = context.dependencies().one_stream::<TestStream>();
        let events = context.dependencies().many_event::<TestEvent>();
        let reported = self.reported.clone();
        Box::pin(async move {
            let stream = stream?;
            let session = stream
                .open(STREAM_OPERATION, "session".to_owned())
                .await?
                .map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("unexpected stream Domain Error: {error}"),
                })?;
            session.send("one".to_owned()).await?;
            let message = match session.receive().await? {
                StreamEvent::Message(message) => message,
                event => {
                    return Err(RuntimeFailure::PluginFailure {
                        detail: format!("unexpected first stream event: {event:?}"),
                    });
                }
            };
            session.close_send().await?;
            let protocol_complete = session.receive().await? == StreamEvent::PeerHalfClosed
                && session.receive().await? == StreamEvent::Terminal(Ok(()));
            drop(session);
            let rejected = stream
                .open(STREAM_OPERATION, "reject".to_owned())
                .await?
                .is_err();
            let cancellation = CancellationToken::new();
            let cancel_after_dispatch = cancellation.clone();
            let opening = stream.open_with_context(
                STREAM_OPERATION,
                InvocationContext::new(900, None, cancellation),
                "slow".to_owned(),
            );
            let (failure, ()) = tokio::join!(
                async {
                    opening
                        .await
                        .expect_err("the slow cross-lane stream open should be cancelled")
                },
                async move {
                    tokio::task::yield_now().await;
                    cancel_after_dispatch.cancel();
                }
            );
            let cancelled = matches!(failure, RuntimeFailure::Cancelled { .. });
            let admissions = events?
                .publish(EVENT_OPERATION, "event-value".to_owned())
                .await
                .into_iter()
                .map(|result| result.admission())
                .collect();
            let _ = reported.send(ConsumerOutcome {
                message,
                protocol_complete,
                rejected,
                cancelled,
                admissions,
            });
            Ok(())
        })
    }
}

#[derive(Debug)]
struct StreamProviderFactory;

impl NativePluginFactory for StreamProviderFactory {
    fn package_id(&self) -> &'static str {
        STREAM_PROVIDER_PACKAGE
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::with_stream_endpoints(
            vec![Rc::new(EchoStreamEndpoint)],
            NoopPluginLifecycle,
        ))
    }
}

#[derive(Debug)]
struct EventProviderFactory {
    reported: mpsc::Sender<(String, String)>,
}

impl NativePluginFactory for EventProviderFactory {
    fn package_id(&self) -> &'static str {
        EVENT_PROVIDER_PACKAGE
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::with_event_endpoints(
            vec![Rc::new(ReportingEventEndpoint {
                reported: self.reported.clone(),
                provider: context.instance_key().to_owned(),
            })],
            NoopPluginLifecycle,
        ))
    }
}

fn interaction_plan() -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("consumer", CONSUMER_PACKAGE)
                .with_execution_lane(ExecutionLaneId::new("frontend"))
                .with_requirement(CapabilityRequirementPlan::one(STREAM_ID, VERSION))
                .with_requirement(CapabilityRequirementPlan::many(EVENT_ID, VERSION)),
            PluginInstancePlan::new("stream-provider", STREAM_PROVIDER_PACKAGE)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new(STREAM_ID, VERSION, [STREAM_OPERATION])
                        .with_stream_operation(STREAM_OPERATION)
                        .with_limits(0, 1)
                        .with_cross_lane_transfer(),
                ),
            PluginInstancePlan::new("event-provider-a", EVENT_PROVIDER_PACKAGE)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new(EVENT_ID, VERSION, [EVENT_OPERATION])
                        .with_event_operation(EVENT_OPERATION)
                        .with_event_capacity(2)
                        .with_cross_lane_transfer(),
                ),
            PluginInstancePlan::new("event-provider-b", EVENT_PROVIDER_PACKAGE)
                .with_execution_lane(ExecutionLaneId::new("workers"))
                .with_capability(
                    CapabilityEndpointPlan::new(EVENT_ID, VERSION, [EVENT_OPERATION])
                        .with_event_operation(EVENT_OPERATION)
                        .with_event_capacity(0)
                        .with_cross_lane_transfer(),
                ),
        ],
        vec![
            CapabilityBinding::new("consumer", STREAM_ID, VERSION, "stream-provider"),
            CapabilityBinding::new("consumer", EVENT_ID, VERSION, "event-provider-a"),
            CapabilityBinding::new("consumer", EVENT_ID, VERSION, "event-provider-b"),
        ],
    )
    .with_execution_lanes(vec![
        ExecutionLanePlan::new("frontend"),
        ExecutionLanePlan::new("workers"),
    ])
    .resolve()
    .expect("transfer-capable cross-lane Stream and Event Plan should resolve")
}

fn interaction_transfers() -> CrossLaneTransferCatalog {
    CrossLaneTransferCatalog::new()
        .with_stream::<TestStream>(&[STREAM_OPERATION])
        .with_event::<TestEvent>(&[EVENT_OPERATION])
}

#[test]
fn cross_lane_stream_and_event_require_registered_send_transfers() {
    let failure = ReplicatedNativeApp::start(interaction_plan(), |_| {
        panic!("adapter creation must not run before transfer validation")
    })
    .expect_err("cross-lane Event types must be registered");
    assert_eq!(
        failure,
        ReplicatedRunnerError::MissingCrossLaneEventTransfer {
            capability: EVENT_ID.to_owned(),
        }
    );

    let failure = ReplicatedNativeApp::start_with_transfer_catalog(
        interaction_plan(),
        |_| panic!("adapter creation must not run before transfer validation"),
        CrossLaneTransferCatalog::new().with_event::<TestEvent>(&[EVENT_OPERATION]),
    )
    .expect_err("cross-lane stream types must be registered");
    assert_eq!(
        failure,
        ReplicatedRunnerError::MissingCrossLaneStreamTransfer {
            capability: STREAM_ID.to_owned(),
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_lifecycle_preserves_cross_lane_stream_protocol_and_event_fanout() {
    let (reported_outcome, outcome) = mpsc::channel();
    let (reported_event, events) = mpsc::channel();
    let app = ReplicatedNativeApp::start_with_transfer_catalog(
        interaction_plan(),
        move |lane| {
            let registry = match lane.as_str() {
                "frontend" => NativePluginRegistry::new().with_factory(ConsumerFactory {
                    reported: reported_outcome.clone(),
                }),
                "workers" => NativePluginRegistry::new()
                    .with_factory(StreamProviderFactory)
                    .with_factory(EventProviderFactory {
                        reported: reported_event.clone(),
                    }),
                other => panic!("unexpected lane {other}"),
            };
            ExecutionAdapterCatalog::single(registry)
        },
        interaction_transfers(),
    )
    .expect("both interaction-complete Kernel lanes should start");

    let outcome = outcome
        .recv_timeout(Duration::from_secs(1))
        .expect("the cross-lane consumer lifecycle should complete");
    assert_eq!(
        outcome,
        ConsumerOutcome {
            message: "session:one".to_owned(),
            protocol_complete: true,
            rejected: true,
            cancelled: true,
            admissions: vec![EventAdmission::Accepted, EventAdmission::Exhausted],
        }
    );
    let delivered = events
        .recv_timeout(Duration::from_secs(1))
        .expect("the admitted Event subscriber should receive the value");
    assert_eq!(
        delivered,
        ("event-provider-a".to_owned(), "event-value".to_owned())
    );
    assert!(events.try_recv().is_err());

    app.shutdown(Duration::from_secs(1))
        .await
        .expect("both lanes should stop cleanly");
}
