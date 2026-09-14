# Workers G1 upstream conformance inventory

Pinned source: `lenso-runtime-conformance` 0.3.2, registry `tests/*.rs`. This inventory maps every upstream test name; it is not a claim that the original deterministic test harness ran in Workers. `Covered` means the stated behavior is exercised by a Workers fixture. Every listed behavior now has a real-host fixture. Real timers replace virtual-clock advancement; deterministic completion order is not promised on Workers.

| Source | Upstream test | Workers status |
| --- | --- | --- |
| `deterministic_schedules.rs` | `cancellation_and_completion_interleavings_preserve_one_terminal_outcome` | Covered by 25 cancellation and 5 real-timer deadline schedules |
| `deterministic_schedules.rs` | `deadline_and_completion_interleavings_remain_deterministic` | Covered by 25 cancellation and 5 real-timer deadline schedules |
| `diagnostics.rs` | `diagnostics_filter_sources_and_drop_overflow_without_affecting_shutdown` | Covered by diagnostics probe; real-clock timing adaptation |
| `diagnostics.rs` | `observer_can_await_the_next_record` | Covered by diagnostics probe; real-clock timing adaptation |
| `diagnostics.rs` | `zero_observers_do_not_change_empty_app_behavior` | Covered by diagnostics probe; real-clock timing adaptation |
| `diagnostics.rs` | `observer_disconnect_and_zero_capacity_are_non_fatal` | Covered by diagnostics probe; real-clock timing adaptation |
| `diagnostics.rs` | `shutdown_records_the_actual_admission_and_cleanup_boundaries` | Covered by diagnostics probe; real-clock timing adaptation |
| `diagnostics.rs` | `diagnostics_do_not_treat_unresolved_caller_text_as_structural_identity` | Covered by diagnostics probe; real-clock timing adaptation |
| `diagnostics.rs` | `request_diagnostics_expose_timing_and_failure_categories_without_domain_bodies` | Covered by diagnostics probe; real-clock timing adaptation |
| `interactions.rs` | `conformance_adapter_exercises_stream_event_and_shutdown_through_one_interface` | Covered by stream/event probe |
| `named_dependencies.rs` | `generated_clients_use_named_views_without_changing_their_interface` | Covered by named dependencies probe |
| `named_dependencies.rs` | `two_named_handles_cannot_bypass_provider_capacity_after_a_terminal_reply` | Covered by named dependencies probe |
| `native_request.rs` | `typed_client_invokes_a_prepared_provider` | Covered by request/registration probes |
| `native_request.rs` | `typed_client_preserves_domain_errors` | Covered by request/registration probes |
| `native_request.rs` | `kernel_rejects_an_unknown_operation_as_a_runtime_failure` | Covered by request/registration probes |
| `native_request.rs` | `typed_client_reports_a_missing_binding_as_a_runtime_failure` | Covered by request/registration probes |
| `native_request.rs` | `kernel_rejects_a_planned_plugin_without_a_linked_factory` | Covered by request/registration probes |
| `native_request.rs` | `kernel_rejects_a_native_plan_with_an_unsupported_schema` | Covered by request/registration probes |
| `native_request.rs` | `conformance_adapter_rejects_an_operation_table_mismatch_during_preparation` | Covered by request/registration probes |
| `native_request.rs` | `composition_materializes_keyed_instances_requirements_and_deterministic_many_bindings` | Covered by bindings probe |
| `native_request.rs` | `missing_one_binding_is_rejected_before_native_boot` | Covered by bindings probe |
| `native_request.rs` | `missing_one_binding_is_rejected_before_the_execution_adapter_runs` | Covered by bindings probe |
| `native_request.rs` | `optional_requirement_may_be_unbound` | Covered by bindings probe |
| `native_request.rs` | `many_requirement_may_be_unbound_and_fan_out_to_nothing` | Covered by bindings probe |
| `native_request.rs` | `a_singular_client_does_not_fallback_to_the_first_many_provider` | Covered by bindings probe |
| `native_request.rs` | `ambiguous_one_binding_is_rejected` | Covered by bindings probe |
| `native_request.rs` | `required_one_bindings_cannot_form_an_activation_cycle` | Covered by bindings probe |
| `native_request.rs` | `invalid_provider_reference_is_rejected` | Covered by bindings probe |
| `native_request.rs` | `incompatible_capability_versions_are_rejected` | Covered by bindings probe |
| `native_request.rs` | `two_providers_pass_the_same_typed_client_contract` | Covered by request/registration probes |
| `native_request.rs` | `replacing_the_provider_changes_composition_and_plan_but_not_the_consumer_binding` | Covered by bindings probe |
| `native_request.rs` | `conformance_adapter_recreates_a_generation_through_the_supervision_seam` | Covered by lifecycle supervision probe |

The schedules probe exercises the upstream 25 cancellation/completion combinations and five deadline/completion combinations with the real Driver. Each must terminate as success or its exact cancellation/deadline failure, then shut down cleanly. This preserves outcome invariants without claiming virtual-clock timing. Stream half-close, terminal/domain errors, event fan-out and post-shutdown rejection use the existing upstream contracts.

Lifecycle rollback, cleanup errors, bounded shutdown, restart exhaustion and Wasm generation abandonment have additional experiment-specific probes described in the adjacent evidence reports.
