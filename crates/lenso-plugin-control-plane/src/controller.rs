use std::{collections::BTreeMap, time::Duration};

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    AppGenerationTransitionSpec, CanonicalDocument, ControlPlaneError, ControlStateStore,
    DurableControlState, DurableGenerationRoute, DurableGenerationSupervisor,
    DurableTransitionOutcome, GenerationMaintenanceOutcome, GenerationRuntime, ResolvedGeneration,
    StateCompatibilityReceipt,
};

/// Operator-visible events emitted by the single App Generation authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GenerationControllerEvent {
    Transitioned(DurableTransitionOutcome),
    Maintained(GenerationMaintenanceOutcome),
    Suspended,
    ShutdownStarted,
    Stopped,
}

enum GenerationCommand<T: Clone + std::fmt::Debug> {
    Transition {
        transition: Box<CanonicalDocument<AppGenerationTransitionSpec>>,
        candidate: Box<ResolvedGeneration>,
        receipts: BTreeMap<String, CanonicalDocument<StateCompatibilityReceipt>>,
        reply: oneshot::Sender<Result<DurableTransitionOutcome, ControlPlaneError>>,
    },
    Route {
        routing_epoch: Option<u64>,
        reply: oneshot::Sender<Result<DurableGenerationRoute<T>, ControlPlaneError>>,
    },
    Rollback {
        standby_digest: String,
        reply: oneshot::Sender<Result<DurableTransitionOutcome, ControlPlaneError>>,
    },
    Inspect {
        reply: oneshot::Sender<DurableControlState>,
    },
    Suspend {
        reply: oneshot::Sender<Result<DurableControlState, ControlPlaneError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<DurableControlState, ControlPlaneError>>,
    },
}

/// Cloneable Host interface for transitions, fenced routes, inspection, and shutdown.
#[derive(Debug)]
pub struct GenerationControllerClient<T: Clone + std::fmt::Debug> {
    commands: mpsc::Sender<GenerationCommand<T>>,
    events: broadcast::Sender<GenerationControllerEvent>,
}

impl<T: Clone + std::fmt::Debug> Clone for GenerationControllerClient<T> {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            events: self.events.clone(),
        }
    }
}

impl<T: Clone + std::fmt::Debug> GenerationControllerClient<T> {
    pub fn subscribe(&self) -> broadcast::Receiver<GenerationControllerEvent> {
        self.events.subscribe()
    }

    pub async fn transition(
        &self,
        transition: CanonicalDocument<AppGenerationTransitionSpec>,
        candidate: ResolvedGeneration,
        receipts: BTreeMap<String, CanonicalDocument<StateCompatibilityReceipt>>,
    ) -> Result<DurableTransitionOutcome, ControlPlaneError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(GenerationCommand::Transition {
                transition: Box::new(transition),
                candidate: Box::new(candidate),
                receipts,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        response.await.map_err(|_| stopped())?
    }

    pub async fn route(&self) -> Result<DurableGenerationRoute<T>, ControlPlaneError> {
        self.route_inner(None).await
    }

    pub async fn route_at_epoch(
        &self,
        routing_epoch: u64,
    ) -> Result<DurableGenerationRoute<T>, ControlPlaneError> {
        self.route_inner(Some(routing_epoch)).await
    }

    async fn route_inner(
        &self,
        routing_epoch: Option<u64>,
    ) -> Result<DurableGenerationRoute<T>, ControlPlaneError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(GenerationCommand::Route {
                routing_epoch,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        response.await.map_err(|_| stopped())?
    }

    pub async fn rollback(
        &self,
        standby_digest: impl Into<String>,
    ) -> Result<DurableTransitionOutcome, ControlPlaneError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(GenerationCommand::Rollback {
                standby_digest: standby_digest.into(),
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        response.await.map_err(|_| stopped())?
    }

    pub async fn inspect(&self) -> Result<DurableControlState, ControlPlaneError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(GenerationCommand::Inspect { reply })
            .await
            .map_err(|_| stopped())?;
        response.await.map_err(|_| stopped())
    }

    /// Stops this Host incarnation without retiring the durable active Generation.
    pub async fn suspend(&self) -> Result<DurableControlState, ControlPlaneError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(GenerationCommand::Suspend { reply })
            .await
            .map_err(|_| stopped())?;
        response.await.map_err(|_| stopped())?
    }

    pub async fn shutdown(&self) -> Result<DurableControlState, ControlPlaneError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(GenerationCommand::Shutdown { reply })
            .await
            .map_err(|_| stopped())?;
        response.await.map_err(|_| stopped())?
    }
}

/// Single-owner loop which advances all App Generation lifecycle authority.
#[derive(Debug)]
pub struct GenerationController<R: GenerationRuntime, S: ControlStateStore> {
    supervisor: DurableGenerationSupervisor<R, S>,
    commands: mpsc::Receiver<GenerationCommand<R::Route>>,
    events: broadcast::Sender<GenerationControllerEvent>,
    maintenance_interval: Duration,
}

impl<R: GenerationRuntime, S: ControlStateStore> GenerationController<R, S> {
    pub fn new(
        supervisor: DurableGenerationSupervisor<R, S>,
        maintenance_interval: Duration,
    ) -> Result<(Self, GenerationControllerClient<R::Route>), ControlPlaneError> {
        if maintenance_interval.is_zero() {
            return Err(ControlPlaneError::HostFailure {
                detail: "Generation maintenance interval must be nonzero".to_owned(),
            });
        }
        let (commands, receiver) = mpsc::channel(64);
        let (events, _) = broadcast::channel(64);
        Ok((
            Self {
                supervisor,
                commands: receiver,
                events: events.clone(),
                maintenance_interval,
            },
            GenerationControllerClient { commands, events },
        ))
    }

    /// Runs until the command channel closes or a client requests graceful shutdown.
    pub async fn run(mut self) -> Result<DurableControlState, ControlPlaneError> {
        let mut interval = tokio::time::interval(self.maintenance_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut shutting_down = false;
        let mut shutdown_replies = Vec::new();
        loop {
            let step = tokio::select! {
                biased;
                _ = interval.tick() => {
                    match now_unix_nanos() {
                        Ok(now) => match self.supervisor.maintain(now).await {
                            Ok(outcomes) => {
                                for outcome in outcomes {
                                    let _ = self.events.send(GenerationControllerEvent::Maintained(outcome));
                                }
                                Ok(None)
                            }
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    }
                }
                command = self.commands.recv(), if !shutting_down => {
                    match command {
                        Some(command) => self.handle_command(command, &mut shutting_down, &mut shutdown_replies).await,
                        None => self.start_shutdown(&mut shutting_down).map(|()| None),
                    }
                }
            };
            match step {
                Err(error) => {
                    for reply in shutdown_replies {
                        let _ = reply.send(Err(error.clone()));
                    }
                    return Err(error);
                }
                Ok(Some(state)) => {
                    let _ = self.events.send(GenerationControllerEvent::Stopped);
                    return Ok(state);
                }
                Ok(None) => {}
            }
            if shutting_down && self.supervisor.is_retired() {
                let state = self.supervisor.state().clone();
                let _ = self.events.send(GenerationControllerEvent::Stopped);
                for reply in shutdown_replies {
                    let _ = reply.send(Ok(state.clone()));
                }
                return Ok(state);
            }
        }
    }

    async fn handle_command(
        &mut self,
        command: GenerationCommand<R::Route>,
        shutting_down: &mut bool,
        shutdown_replies: &mut Vec<oneshot::Sender<Result<DurableControlState, ControlPlaneError>>>,
    ) -> Result<Option<DurableControlState>, ControlPlaneError> {
        match command {
            GenerationCommand::Transition {
                transition,
                candidate,
                receipts,
                reply,
            } => {
                let result = self
                    .supervisor
                    .transition(&transition, &candidate, &receipts, now_unix_nanos()?)
                    .await;
                if let Ok(outcome) = &result {
                    let _ = self
                        .events
                        .send(GenerationControllerEvent::Transitioned(outcome.clone()));
                }
                let _ = reply.send(result);
            }
            GenerationCommand::Route {
                routing_epoch,
                reply,
            } => {
                let result = routing_epoch.map_or_else(
                    || self.supervisor.route(),
                    |epoch| self.supervisor.route_at_epoch(epoch),
                );
                let _ = reply.send(result);
            }
            GenerationCommand::Rollback {
                standby_digest,
                reply,
            } => {
                let result = self.supervisor.rollback(&standby_digest, now_unix_nanos()?);
                if let Ok(outcome) = &result {
                    let _ = self
                        .events
                        .send(GenerationControllerEvent::Transitioned(outcome.clone()));
                }
                let _ = reply.send(result);
            }
            GenerationCommand::Inspect { reply } => {
                let _ = reply.send(self.supervisor.state().clone());
            }
            GenerationCommand::Suspend { reply } => match self.supervisor.suspend_host().await {
                Ok(state) => {
                    let _ = self.events.send(GenerationControllerEvent::Suspended);
                    let _ = reply.send(Ok(state.clone()));
                    return Ok(Some(state));
                }
                Err(error @ ControlPlaneError::TransitionRejected { .. }) => {
                    let _ = reply.send(Err(error));
                }
                Err(error) => {
                    let _ = reply.send(Err(error.clone()));
                    return Err(error);
                }
            },
            GenerationCommand::Shutdown { reply } => {
                shutdown_replies.push(reply);
                self.start_shutdown(shutting_down)?;
            }
        }
        Ok(None)
    }

    fn start_shutdown(&mut self, shutting_down: &mut bool) -> Result<(), ControlPlaneError> {
        if !*shutting_down {
            self.supervisor.begin_shutdown(now_unix_nanos()?)?;
            *shutting_down = true;
            self.commands.close();
            let _ = self.events.send(GenerationControllerEvent::ShutdownStarted);
        }
        Ok(())
    }
}

fn now_unix_nanos() -> Result<u128, ControlPlaneError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|error| ControlPlaneError::HostFailure {
            detail: format!("system clock precedes Unix epoch: {error}"),
        })
}

fn stopped() -> ControlPlaneError {
    ControlPlaneError::HostFailure {
        detail: "Generation Controller is stopped".to_owned(),
    }
}
