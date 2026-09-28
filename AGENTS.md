# Agent instructions

This checkout retains pre-consolidation history. New Rust framework work belongs
in `LioRael/lenso` under ADR 0077; the rules below cover maintenance here.

This repository owns host Runtime Drivers, Execution Adapters, and Runner
orchestration. Consume the published portable core and prove implementations
through `lenso-runtime-conformance`; do not move Kernel semantics here.

Use Conventional Commits and locked workspace checks.
