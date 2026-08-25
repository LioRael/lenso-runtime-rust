use std::{cell::RefCell, future::Future, rc::Rc};

use lenso_kernel::{
    CancellationToken, ManagedTask, ManagedTaskError, ManagedTaskScope, RuntimeFailure,
};

/// A Module field connected to its generation-owned task scope during activation.
///
/// Declare this as `#[tasks] tasks: ManagedTasks` on a struct-level Module. The authoring
/// macro connects it before the Module's optional `Lifecycle::activate` hook runs.
#[derive(Clone, Default)]
pub struct ManagedTasks {
    scope: Rc<RefCell<Option<ManagedTaskScope>>>,
}

impl std::fmt::Debug for ManagedTasks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedTasks")
            .field("active", &self.is_active())
            .finish()
    }
}

impl ManagedTasks {
    /// Returns whether the Module has entered activation and received its task scope.
    pub fn is_active(&self) -> bool {
        self.scope.borrow().is_some()
    }

    /// Returns the cooperative cancellation token for the active Module generation.
    pub fn cancellation(&self) -> Result<CancellationToken, ManagedTasksError> {
        self.scope
            .borrow()
            .as_ref()
            .map(ManagedTaskScope::cancellation)
            .ok_or(ManagedTasksError::Inactive)
    }

    /// Spawns work owned by this Module Instance generation.
    pub fn spawn_local(
        &self,
        task: impl Future<Output = ()> + 'static,
    ) -> Result<ManagedTask, ManagedTasksError> {
        let scope = self
            .scope
            .borrow()
            .clone()
            .ok_or(ManagedTasksError::Inactive)?;
        scope
            .spawn_local(Box::pin(task))
            .map_err(ManagedTasksError::Scope)
    }

    #[doc(hidden)]
    pub fn __lenso_connect(&self, scope: ManagedTaskScope) -> Result<(), RuntimeFailure> {
        let mut active = self.scope.borrow_mut();
        if active.is_some() {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "managed task field was connected more than once".to_owned(),
            });
        }
        *active = Some(scope);
        Ok(())
    }

    #[doc(hidden)]
    pub fn __lenso_disconnect(&self) {
        self.scope.borrow_mut().take();
    }
}

/// Failure returned when a Module cannot spawn generation-owned work.
#[derive(Debug)]
pub enum ManagedTasksError {
    /// The Module has not entered activation.
    Inactive,
    /// The connected Kernel task scope rejected the task.
    Scope(ManagedTaskError),
}
