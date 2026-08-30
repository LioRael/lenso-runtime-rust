use std::{
    collections::BTreeMap,
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::Mutex,
};

use lenso_kernel::RuntimeFailure;
use lenso_runtime_codec::{ArtifactHandle, JsonInvocationOutcome};

use crate::DylibLimits;

pub const ABI_VERSION: u32 = 1;
pub const STATUS_OK: u32 = 0;
pub const STATUS_DOMAIN_ERROR: u32 = 1;
const STATUS_FATAL: u32 = 2;

/// Buffer allocated and owned by the host allocator callbacks.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct LensoBufferV1 {
    pub pointer: *mut u8,
    pub length: usize,
    pub capacity: usize,
}

/// Host allocator table supplied to the dylib entry symbol.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct LensoHostV1 {
    pub abi_version: u32,
    pub struct_size: usize,
    pub allocator_context: *mut c_void,
    pub allocate: Option<unsafe extern "C" fn(*mut c_void, usize) -> LensoBufferV1>,
    pub reallocate:
        Option<unsafe extern "C" fn(*mut c_void, LensoBufferV1, usize) -> LensoBufferV1>,
    pub free: Option<unsafe extern "C" fn(*mut c_void, LensoBufferV1) -> u32>,
    pub max_result_bytes: usize,
    pub reserved: [usize; 8],
}

impl std::fmt::Debug for LensoHostV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LensoHostV1")
            .field("abi_version", &self.abi_version)
            .field("struct_size", &self.struct_size)
            .field("max_result_bytes", &self.max_result_bytes)
            .finish_non_exhaustive()
    }
}

/// Versioned root function table returned by the single `lenso_plugin_v1` symbol.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct LensoPluginV1 {
    pub abi_version: u32,
    pub struct_size: usize,
    pub plugin_context: *mut c_void,
    pub descriptor_json: *const u8,
    pub descriptor_json_len: usize,
    pub invoke: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const u8,
            usize,
            *const u8,
            usize,
            *const u8,
            usize,
            *mut LensoBufferV1,
        ) -> u32,
    >,
    pub shutdown: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    pub reserved: [usize; 8],
}

impl std::fmt::Debug for LensoPluginV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LensoPluginV1")
            .field("abi_version", &self.abi_version)
            .field("struct_size", &self.struct_size)
            .field("descriptor_json_len", &self.descriptor_json_len)
            .finish_non_exhaustive()
    }
}

type EntryPoint = unsafe extern "C" fn(*const LensoHostV1, *mut LensoPluginV1) -> u32;

#[derive(Clone, Debug)]
pub(crate) struct CapabilityAbiDescriptor {
    pub capability_id: &'static str,
    pub descriptor_version: &'static str,
    pub request_operations: Vec<String>,
}

#[derive(Debug, Default)]
struct AllocatorState {
    allocations: Mutex<BTreeMap<usize, Vec<u8>>>,
}

pub(crate) struct LoadedDylib {
    _artifact: ArtifactHandle,
    root: LensoPluginV1,
    allocator: Box<AllocatorState>,
    limits: DylibLimits,
    failed: std::cell::Cell<bool>,
    shutdown: std::cell::Cell<bool>,
}

impl std::fmt::Debug for LoadedDylib {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedDylib")
            .field("root", &self.root)
            .field("failed", &self.failed)
            .field("shutdown", &self.shutdown)
            .finish_non_exhaustive()
    }
}

impl LoadedDylib {
    pub(crate) fn load(
        artifact: ArtifactHandle,
        capabilities: &[CapabilityAbiDescriptor],
        limits: DylibLimits,
    ) -> Result<Self, RuntimeFailure> {
        let mut allocator = Box::<AllocatorState>::default();
        let host = LensoHostV1 {
            abi_version: ABI_VERSION,
            struct_size: size_of::<LensoHostV1>(),
            allocator_context: ptr::from_mut(allocator.as_mut()).cast::<c_void>(),
            allocate: Some(host_allocate),
            reallocate: Some(host_reallocate),
            free: Some(host_free),
            max_result_bytes: limits.max_result_bytes,
            reserved: [0; 8],
        };
        // SAFETY: loading foreign native code is the explicit trust seam of this Adapter. The
        // exact bytes were digest-verified and approved by `DylibVerifier` before this call.
        let library = unsafe { libloading::Library::new(artifact.path()) }.map_err(abi_failure)?;
        // SAFETY: the only accepted public symbol has the documented V1 C function type.
        let entry =
            unsafe { library.get::<EntryPoint>(b"lenso_plugin_v1\0") }.map_err(abi_failure)?;
        let mut root = LensoPluginV1 {
            abi_version: 0,
            struct_size: 0,
            plugin_context: ptr::null_mut(),
            descriptor_json: ptr::null(),
            descriptor_json_len: 0,
            invoke: None,
            shutdown: None,
            reserved: [usize::MAX; 8],
        };
        // SAFETY: `entry` was resolved with the V1 C type and receives valid table pointers for
        // the duration of the call. Foreign unwinding remains forbidden by the ABI contract.
        let status = catch_unwind(AssertUnwindSafe(|| unsafe {
            entry(&raw const host, &raw mut root)
        }))
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "native dylib panicked while constructing its root table".to_owned(),
        })?;
        if status != STATUS_OK {
            return plugin_failure("native dylib rejected V1 host table");
        }
        validate_root(&root, &limits)?;
        let descriptor_bytes = if root.descriptor_json_len == 0 {
            &[][..]
        } else {
            // SAFETY: root validation bounded the declared length and requires non-null. The
            // trusted library promises this static descriptor remains valid while loaded.
            unsafe { std::slice::from_raw_parts(root.descriptor_json, root.descriptor_json_len) }
        };
        validate_descriptor(capabilities, descriptor_bytes)?;
        // Native libraries are deliberately never unloaded from a live Host. Function tables,
        // TLS, descendant threads, and foreign runtimes make safe unload unprovable.
        std::mem::forget(library);
        Ok(Self {
            _artifact: artifact,
            root,
            allocator,
            limits,
            failed: std::cell::Cell::new(false),
            shutdown: std::cell::Cell::new(false),
        })
    }

    pub(crate) fn invoke(
        &self,
        capability: &str,
        operation: &str,
        request: &[u8],
    ) -> Result<JsonInvocationOutcome, RuntimeFailure> {
        if self.failed.get() || self.shutdown.get() {
            return plugin_failure("native dylib generation is retired");
        }
        if request.len() > self.limits.max_request_bytes {
            return plugin_failure("native dylib request exceeds max_request_bytes");
        }
        let invoke = self.root.invoke.expect("root validation requires invoke");
        let mut output = LensoBufferV1 {
            pointer: ptr::null_mut(),
            length: 0,
            capacity: 0,
        };
        // SAFETY: all input slices remain valid for the call and `output` is a valid out-parameter.
        // The library is explicitly trusted and foreign unwinding is forbidden by contract.
        let status = catch_unwind(AssertUnwindSafe(|| unsafe {
            invoke(
                self.root.plugin_context,
                capability.as_ptr(),
                capability.len(),
                operation.as_ptr(),
                operation.len(),
                request.as_ptr(),
                request.len(),
                &raw mut output,
            )
        }))
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "native dylib panicked across its invoke callback".to_owned(),
        })?;
        let bytes = self.take_output(output)?;
        let value =
            serde_json::from_slice(&bytes).map_err(|error| RuntimeFailure::PluginFailure {
                detail: bounded(format!("native dylib returned invalid JSON: {error}")),
            })?;
        match status {
            STATUS_OK => Ok(JsonInvocationOutcome::Success(value)),
            STATUS_DOMAIN_ERROR => Ok(JsonInvocationOutcome::DomainError(value)),
            _ => {
                self.failed.set(true);
                plugin_failure("native dylib returned a fatal status")
            }
        }
    }

    pub(crate) fn shutdown(&self) -> Result<(), RuntimeFailure> {
        if self.shutdown.replace(true) {
            return Ok(());
        }
        let Some(shutdown) = self.root.shutdown else {
            return Ok(());
        };
        // SAFETY: the validated V1 root owns this callback and context. The callback is invoked
        // at most once before allocator state is dropped.
        let status = catch_unwind(AssertUnwindSafe(|| unsafe {
            shutdown(self.root.plugin_context)
        }))
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "native dylib panicked across its shutdown callback".to_owned(),
        })?;
        if status != STATUS_OK {
            return plugin_failure("native dylib shutdown returned a fatal status");
        }
        if !self
            .allocator
            .allocations
            .lock()
            .expect("allocator lock")
            .is_empty()
        {
            return plugin_failure("native dylib leaked host-owned output buffers");
        }
        Ok(())
    }

    fn take_output(&self, output: LensoBufferV1) -> Result<Vec<u8>, RuntimeFailure> {
        if output.length > self.limits.max_result_bytes || output.length > output.capacity {
            self.failed.set(true);
            return plugin_failure("native dylib returned an invalid output buffer length");
        }
        let mut allocations = self.allocator.allocations.lock().expect("allocator lock");
        let mut allocation = allocations
            .remove(&(output.pointer as usize))
            .ok_or_else(|| RuntimeFailure::PluginFailure {
                detail: "native dylib returned a buffer not owned by the host allocator".to_owned(),
            })?;
        if allocation.capacity() != output.capacity {
            self.failed.set(true);
            return plugin_failure("native dylib changed output buffer ownership metadata");
        }
        allocation.truncate(output.length);
        Ok(allocation)
    }
}

impl Drop for LoadedDylib {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn validate_root(root: &LensoPluginV1, limits: &DylibLimits) -> Result<(), RuntimeFailure> {
    if root.abi_version != ABI_VERSION
        || root.struct_size != size_of::<LensoPluginV1>()
        || root.invoke.is_none()
        || root.reserved != [0; 8]
        || root.descriptor_json_len > limits.max_descriptor_bytes
        || (root.descriptor_json_len != 0 && root.descriptor_json.is_null())
    {
        return plugin_failure("native dylib returned an invalid V1 root table");
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DylibDescriptor {
    capabilities: Vec<DylibCapability>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DylibCapability {
    capability_id: String,
    descriptor_version: String,
    request_operations: Vec<String>,
}

fn validate_descriptor(
    capabilities: &[CapabilityAbiDescriptor],
    bytes: &[u8],
) -> Result<(), RuntimeFailure> {
    let descriptor: DylibDescriptor =
        serde_json::from_slice(bytes).map_err(|error| RuntimeFailure::InvalidResolvedPlan {
            detail: bounded(format!("native dylib descriptor is invalid: {error}")),
        })?;
    if descriptor.capabilities.len() != capabilities.len() {
        return plugin_failure("native dylib Capability table does not match the Plan");
    }
    for (declared, capability) in descriptor.capabilities.iter().zip(capabilities) {
        if declared.capability_id != capability.capability_id
            || declared.descriptor_version != capability.descriptor_version
            || declared.request_operations != capability.request_operations
        {
            return Err(RuntimeFailure::ProtocolViolation {
                capability: capability.capability_id,
            });
        }
    }
    Ok(())
}

unsafe extern "C" fn host_allocate(context: *mut c_void, capacity: usize) -> LensoBufferV1 {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || capacity == 0 {
            return empty_buffer();
        }
        // SAFETY: context points to the live `AllocatorState` owned by `LoadedDylib`.
        let state = unsafe { &*(context.cast::<AllocatorState>()) };
        // Initialize the complete host-owned buffer. The trusted library chooses the returned
        // logical length, but the host never lets that foreign assertion make uninitialized Rust
        // memory observable.
        let mut allocation = vec![0_u8; capacity];
        let pointer = allocation.as_mut_ptr();
        state
            .allocations
            .lock()
            .expect("allocator lock")
            .insert(pointer as usize, allocation);
        LensoBufferV1 {
            pointer,
            length: 0,
            capacity,
        }
    }))
    .unwrap_or_else(|_| empty_buffer())
}

unsafe extern "C" fn host_reallocate(
    context: *mut c_void,
    buffer: LensoBufferV1,
    new_capacity: usize,
) -> LensoBufferV1 {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() || new_capacity < buffer.length {
            return empty_buffer();
        }
        // SAFETY: context points to the live `AllocatorState` owned by `LoadedDylib`.
        let state = unsafe { &*(context.cast::<AllocatorState>()) };
        let mut allocations = state.allocations.lock().expect("allocator lock");
        let Some(mut allocation) = allocations.remove(&(buffer.pointer as usize)) else {
            return empty_buffer();
        };
        if allocation.capacity() != buffer.capacity {
            allocations.insert(allocation.as_mut_ptr() as usize, allocation);
            return empty_buffer();
        }
        if new_capacity > allocation.len() {
            allocation.resize(new_capacity, 0);
        }
        let pointer = allocation.as_mut_ptr();
        let capacity = allocation.capacity();
        allocations.insert(pointer as usize, allocation);
        LensoBufferV1 {
            pointer,
            length: buffer.length,
            capacity,
        }
    }))
    .unwrap_or_else(|_| empty_buffer())
}

unsafe extern "C" fn host_free(context: *mut c_void, buffer: LensoBufferV1) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            return STATUS_FATAL;
        }
        // SAFETY: context points to the live `AllocatorState` owned by `LoadedDylib`.
        let state = unsafe { &*(context.cast::<AllocatorState>()) };
        let removed = state
            .allocations
            .lock()
            .expect("allocator lock")
            .remove(&(buffer.pointer as usize));
        match removed {
            Some(allocation) if allocation.capacity() == buffer.capacity => STATUS_OK,
            _ => STATUS_FATAL,
        }
    }))
    .unwrap_or(STATUS_FATAL)
}

const fn empty_buffer() -> LensoBufferV1 {
    LensoBufferV1 {
        pointer: ptr::null_mut(),
        length: 0,
        capacity: 0,
    }
}

fn abi_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: bounded(format!("native dylib ABI failure: {error}")),
    }
}

fn plugin_failure<T>(detail: impl Into<String>) -> Result<T, RuntimeFailure> {
    Err(RuntimeFailure::PluginFailure {
        detail: bounded(detail.into()),
    })
}

fn bounded(mut detail: String) -> String {
    const MAX_DETAIL: usize = 1024;
    if detail.len() > MAX_DETAIL {
        let mut boundary = MAX_DETAIL;
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        detail.truncate(boundary);
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::bounded;

    #[test]
    fn bounded_failure_preserves_utf8() {
        let detail = bounded("界".repeat(400));

        assert_eq!(detail.len(), 1023);
        assert_eq!(detail.chars().count(), 341);
    }
}
