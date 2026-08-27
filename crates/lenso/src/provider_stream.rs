use std::{any::Any, cell::Cell, fmt, marker::PhantomData, rc::Rc};

use futures::{
    SinkExt, StreamExt,
    channel::mpsc,
    future::{Either, LocalBoxFuture, select},
    lock::Mutex,
};
use lenso_kernel::{
    CancellationToken, InvocationContext, NativeStreamItem, NativeStreamSession, RuntimeFailure,
    StreamCapability,
};

use crate::PluginResult;

/// One typed value sent by a Capability consumer to a Stream provider.
#[derive(Debug)]
pub enum StreamInput<C: StreamCapability> {
    /// One ordered Capability message.
    Message(C::Message),
    /// The consumer closed its sending direction while retaining its receive direction.
    PeerHalfClosed,
}

enum ProviderOutput<C: StreamCapability> {
    Message(C::Message),
    PeerHalfClosed,
    Terminal(Result<(), C::DomainError>),
    Runtime(RuntimeFailure),
}

/// The typed provider side of one bounded bidirectional Stream session.
///
/// Plugin code uses this channel to exchange Capability values. Generated
/// lowering keeps type erasure and [`NativeStreamSession`] behind the facade.
pub struct ProviderStreamChannel<C: StreamCapability> {
    outgoing: mpsc::Sender<ProviderOutput<C>>,
    incoming: mpsc::Receiver<StreamInput<C>>,
    cancellation: CancellationToken,
    send_closed: bool,
    terminated: bool,
}

impl<C: StreamCapability> fmt::Debug for ProviderStreamChannel<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderStreamChannel")
            .field("capability", &C::ID)
            .field("send_closed", &self.send_closed)
            .field("terminated", &self.terminated)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl<C: StreamCapability> ProviderStreamChannel<C> {
    /// Sends one typed message with bounded backpressure.
    pub async fn send(&mut self, message: C::Message) -> Result<(), RuntimeFailure> {
        if self.send_closed || self.terminated {
            return Err(RuntimeFailure::AdmissionClosed);
        }
        self.send_output(ProviderOutput::Message(message)).await
    }

    /// Closes only the provider's sending direction.
    pub async fn close_send(&mut self) -> Result<(), RuntimeFailure> {
        if self.send_closed || self.terminated {
            return Err(RuntimeFailure::AdmissionClosed);
        }
        self.send_closed = true;
        self.send_output(ProviderOutput::PeerHalfClosed).await
    }

    /// Completes the Stream successfully.
    pub async fn finish(&mut self) -> Result<(), RuntimeFailure> {
        self.terminate(ProviderOutput::Terminal(Ok(()))).await
    }

    /// Completes the Stream with a Capability-defined Domain Error.
    pub async fn fail(&mut self, error: C::DomainError) -> Result<(), RuntimeFailure> {
        self.terminate(ProviderOutput::Terminal(Err(error))).await
    }

    /// Completes the Stream with an infrastructure Runtime Failure.
    pub async fn fail_runtime(&mut self, error: RuntimeFailure) -> Result<(), RuntimeFailure> {
        self.terminate(ProviderOutput::Runtime(error)).await
    }

    /// Closes the provider send direction and completes the Stream exactly once.
    ///
    /// Consuming the channel prevents Plugin code from accidentally sending or
    /// terminating the session again after its operation result is known.
    pub async fn complete(
        mut self,
        result: PluginResult<(), C::DomainError>,
    ) -> Result<(), RuntimeFailure> {
        if !self.send_closed {
            self.close_send().await?;
        }
        match result {
            Ok(()) => self.finish().await,
            Err(crate::PluginError::Domain(error)) => self.fail(error).await,
            Err(crate::PluginError::Runtime(error)) => self.fail_runtime(error).await,
        }
    }

    /// Receives the next typed consumer message or half-close marker.
    pub async fn receive(&mut self) -> Result<StreamInput<C>, RuntimeFailure> {
        if self.cancellation.is_cancelled() {
            return Err(RuntimeFailure::AdmissionClosed);
        }
        let receive = self.incoming.next();
        futures::pin_mut!(receive);
        match select(receive, self.cancellation.cancelled()).await {
            Either::Left((Some(input), _)) => Ok(input),
            Either::Left((None, _)) | Either::Right(_) => Err(RuntimeFailure::AdmissionClosed),
        }
    }

    /// Returns whether the consumer cancelled this Stream.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    async fn terminate(&mut self, output: ProviderOutput<C>) -> Result<(), RuntimeFailure> {
        if self.terminated {
            return Err(RuntimeFailure::AdmissionClosed);
        }
        self.terminated = true;
        self.send_output(output).await
    }

    async fn send_output(&mut self, output: ProviderOutput<C>) -> Result<(), RuntimeFailure> {
        if self.cancellation.is_cancelled() {
            return Err(RuntimeFailure::AdmissionClosed);
        }
        let send = self.outgoing.send(output);
        futures::pin_mut!(send);
        match select(send, self.cancellation.cancelled()).await {
            Either::Left((Ok(()), _)) => Ok(()),
            Either::Left((Err(_), _)) | Either::Right(_) => Err(RuntimeFailure::AdmissionClosed),
        }
    }
}

/// One typed provider Stream erased to the native Adapter only after authoring.
pub struct ProviderStream<C: StreamCapability> {
    incoming: mpsc::Sender<StreamInput<C>>,
    outgoing: Rc<Mutex<mpsc::Receiver<ProviderOutput<C>>>>,
    cancellation: CancellationToken,
    consumer_send_closed: Rc<Cell<bool>>,
    terminated: Rc<Cell<bool>>,
    marker: PhantomData<fn() -> C>,
}

impl<C: StreamCapability> ProviderStream<C> {
    /// Creates one bounded typed Stream tied to the invocation's cancellation.
    pub fn channel(
        context: &InvocationContext,
        capacity: usize,
    ) -> (Self, ProviderStreamChannel<C>) {
        let (incoming_sender, incoming_receiver) = mpsc::channel(capacity);
        let (outgoing_sender, outgoing_receiver) = mpsc::channel(capacity);
        let cancellation = context.cancellation();
        (
            Self {
                incoming: incoming_sender,
                outgoing: Rc::new(Mutex::new(outgoing_receiver)),
                cancellation: cancellation.clone(),
                consumer_send_closed: Rc::new(Cell::new(false)),
                terminated: Rc::new(Cell::new(false)),
                marker: PhantomData,
            },
            ProviderStreamChannel {
                outgoing: outgoing_sender,
                incoming: incoming_receiver,
                cancellation,
                send_closed: false,
                terminated: false,
            },
        )
    }
}

impl<C: StreamCapability> fmt::Debug for ProviderStream<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderStream")
            .field("capability", &C::ID)
            .field("consumer_send_closed", &self.consumer_send_closed.get())
            .field("terminated", &self.terminated.get())
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl<C: StreamCapability> NativeStreamSession for ProviderStream<C> {
    fn send(&self, message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        if self.cancellation.is_cancelled() || self.terminated.get() {
            return Box::pin(futures::future::ready(Err(RuntimeFailure::AdmissionClosed)));
        }
        if self.consumer_send_closed.get() {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        }
        let Ok(message) = message.downcast::<C::Message>() else {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        };
        let mut incoming = self.incoming.clone();
        let cancellation = self.cancellation.clone();
        Box::pin(async move {
            let send = incoming.send(StreamInput::Message(*message));
            futures::pin_mut!(send);
            match select(send, cancellation.cancelled()).await {
                Either::Left((Ok(()), _)) => Ok(()),
                Either::Left((Err(_), _)) | Either::Right(_) => {
                    Err(RuntimeFailure::AdmissionClosed)
                }
            }
        })
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        if self.cancellation.is_cancelled() || self.terminated.get() {
            return Box::pin(futures::future::ready(Err(RuntimeFailure::AdmissionClosed)));
        }
        let outgoing = Rc::clone(&self.outgoing);
        let cancellation = self.cancellation.clone();
        let terminated = Rc::clone(&self.terminated);
        Box::pin(async move {
            let receive = async move { outgoing.lock().await.next().await };
            futures::pin_mut!(receive);
            match select(receive, cancellation.cancelled()).await {
                Either::Left((Some(ProviderOutput::Message(message)), _)) => {
                    Ok(NativeStreamItem::Message(Box::new(message)))
                }
                Either::Left((Some(ProviderOutput::PeerHalfClosed), _)) => {
                    Ok(NativeStreamItem::PeerHalfClosed)
                }
                Either::Left((Some(ProviderOutput::Terminal(result)), _)) => {
                    terminated.set(true);
                    Ok(NativeStreamItem::Terminal(
                        result.map_err(|error| Box::new(error) as Box<dyn Any>),
                    ))
                }
                Either::Left((Some(ProviderOutput::Runtime(error)), _)) => {
                    terminated.set(true);
                    Err(error)
                }
                Either::Left((None, _)) => Err(RuntimeFailure::PluginFailure {
                    detail: format!("provider Stream {} ended without a terminal outcome", C::ID),
                }),
                Either::Right(_) => Err(RuntimeFailure::AdmissionClosed),
            }
        })
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        if self.cancellation.is_cancelled() || self.terminated.get() {
            return Box::pin(futures::future::ready(Err(RuntimeFailure::AdmissionClosed)));
        }
        if self.consumer_send_closed.replace(true) {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::ProtocolViolation { capability: C::ID },
            )));
        }
        let mut incoming = self.incoming.clone();
        let cancellation = self.cancellation.clone();
        Box::pin(async move {
            let send = incoming.send(StreamInput::PeerHalfClosed);
            futures::pin_mut!(send);
            match select(send, cancellation.cancelled()).await {
                Either::Left((Ok(()), _)) => Ok(()),
                Either::Left((Err(_), _)) | Either::Right(_) => {
                    Err(RuntimeFailure::AdmissionClosed)
                }
            }
        })
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}
