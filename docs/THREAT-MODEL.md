# Threat model

Control protects host files, provider credentials, operator authority, published
checkpoints and external repositories from untrusted model output, project code,
build scripts, web content, room posts and delegated results. The host OS, Linux
VM/kernel, container engine, Nix inputs, Control broker and provider harness
binaries are trusted dependencies. A compromised host or container runtime is
outside the isolation guarantee; containers and reproducible builds do not prove
arbitrary downloaded code safe.

## Enforced boundaries and evidence

- **Identity and host resources.** Node policy uses canonical Discord IDs and
  recorded DM bindings, independently of model text or display names. Worker
  requests cannot supply operator identity. Opening a host directory records an
  intent; a separate authenticated operator action approves an isolated import.
  Imports skip symlinks without following targets, exclude known credential/generated paths and isolate
  Git metadata from the original checkout. Lenses compose context and tools;
  they do not grant authority. Evidence:
  [identity.rs](../src/identity.rs),
  `roles_bound_actions_and_workspaces`,
  `direct_messages_need_matching_recipient_bindings_and_keep_roles`;
  [projects.rs](../src/projects.rs),
  `project_request_is_only_intent_and_cannot_replace_an_approved_grant`,
  `import_ignores_links_to_files_directories_missing_paths_and_cycles`.

- **Credentials and executable code.** Codex credentials are read-only mounts
  into its account-bearing harness. Its inner execution sandbox denies credential
  paths and network access; a canary/PID-namespace probe must pass before real
  credentials are mounted. Its seccomp profile and relaxed masked-path setting
  support that inner sandbox; the probe is required, not an optional fallback.
  Claude's account-bearing harness receives a mailbox,
  with native tools disabled; project commands execute in separate credential-free
  containers. Provider harnesses need provider network access. Offline execution
  containers have no external network or host daemon sockets; brokered search and
  GitHub are separate network capabilities. Evidence:
  [worker.rs](../src/worker.rs),
  `credentials_are_readonly_mounts_and_never_environment_values`, `sandbox_probe`;
  [claude.rs](../src/claude.rs),
  `live_offline_workspace_isolated_and_persistent_across_processes`.

- **Malicious builds and base-image integrity.** Pinned Nix images run non-root
  with dropped capabilities, no-new-privileges and resource limits. Lite and
  credential-bearing harnesses keep read-only roots. Opt-in Full tool containers
  have private writable layers for single-user Nix; root-owned preinstalled store
  paths and a sticky store directory protect trusted executables. Store additions
  disappear with the container and never modify the shared image. The captured
  profile controls restart verification; old records default to Lite.
  No host Nix daemon is exposed. Offline vendoring prevents an execution job from fetching arbitrary
  dependencies; it does not make existing dependencies or build scripts benign.
  Evidence: [command_jobs.rs](../src/command_jobs.rs),
  `immutable_job_identity_and_isolation_are_checked_before_adoption`,
  `live_commands_stream_past_short_deadline_cancel_and_reconcile_exact_restart_identity`;
  [worker.rs](../src/worker.rs), `container_command_scoped`;
  [worker_profile.rs](../src/worker_profile.rs),
  `live_private_nix_build_and_runtime_isolation`;
  [worker-seccomp.json](../hosting/worker-seccomp.json).

- **Delegation and judges.** Admission checks actor, attempt and lineage; depth
  is limited to two, with a configurable shared root budget and one judge review.
  Nonblocking batches reserve their entire size before acknowledgement, with a
  separate bounded node backlog; materialization never increases that reservation.
  [Spawning](../src/spawning.rs) tests 50-task restart/admission and cancellation.
  Delegation
  checkpoints files and releases its bounded execution slot. Judges use a
  captured parent provider/model, receive read-only source and cannot delegate,
  publish or advance a development head. They can report findings and collaborate
  in rooms. Descendant result reads follow actual delegation/continuation edges,
  enforce actor/root scope and use immutable published artifacts, never live host
  workspaces. Feedback does not automatically apply changes. Evidence:
  [delegation.rs](../src/delegation.rs),
  `denied_identity_mismatched_actor_depth_and_total_budget_are_enforced`,
  `judge_inherits_captured_model_and_has_task_ceiling`;
  [worker_tools.rs](../src/worker_tools.rs),
  `judge_tools_are_enforced_even_when_unadvertised_names_are_forged`;
  [worker.rs](../src/worker.rs),
  `live_judge_source_is_readonly_in_both_execution_adapters`;
  [delegated_results.rs](../src/delegated_results.rs),
  `sibling_and_unrelated_same_actor_chats_do_not_gain_access`.

- **Recovery and publication fences.** Command intent is saved before creation,
  and its immutable container ID before execution. Stops verify recorded
  ownership; unfinished writers must quiesce before checkpoints. Restart marks
  interrupted work without replaying effects. Bounded diagnostics remain in state.
  Evidence: [lifecycle.rs](../src/lifecycle.rs),
  `ownership_checks_every_field_before_termination`,
  `injected_engine_races_failures_and_exact_id_termination`;
  [command_jobs.rs](../src/command_jobs.rs),
  `checkpoint_barrier_fails_closed_for_unconfirmed_writer`;
  [storage.rs](../src/storage.rs),
  `restart_fences_running_and_cancelling_without_replaying_or_publishing`.

  Service commands bind to an owned service's exact verified container ID,
  generation, image, user and private workspace; they cannot supply host paths or
  Docker options. Intent and execution receipts are durable before launch.
  Timeout or uncertain execution requests service stop, and restart never replays
  an in-flight command. Bounded final logs are saved before service removal.
  Evidence: [service commands](../src/services/commands.rs),
  `restart_never_replays_uncertain_execution_and_save_failure_rolls_back`,
  `live_service_commands_download_and_connect_two_private_services`.

- **GitHub side effects.** A private broker credential is bound to the operator's
  configured repository/source. Only completed same-chat improvements with captured
  source/checkpoints are eligible. Durable publication intent precedes reachable
  branch/PR creation; replay reconciles acknowledgements. Changed bases or replaced
  branches are rejected. The tool creates draft PRs, without merge, force-push or
  deployment authority. Evidence: [github.rs](../src/github.rs),
  `only_completed_same_chat_improvements_can_be_published`,
  `disk_failure_prevents_a_reachable_branch_or_pull_request`,
  `lost_branch_or_pr_acknowledgement_is_reconciled_without_duplicate_pr`,
  `diverged_remote_base_prevents_every_external_write`.

- **Browser and preview origins.** Local previews use credential-free offline
  containers, loopback listeners and separate origins. Their proxy rejects Control
  cookies, incorrect Host headers and ambiguous headers. This is not unrestricted
  browser automation. Evidence: [previews.rs](../src/previews.rs),
  `preview_origin_gate_rejects_control_hosts_cookies_and_ambiguous_headers`,
  `live_preview_is_local_readonly_credential_free_and_survives_run_completion`.

- **Scoped service TCP.** Port approval binds a verified service, owner, node,
  generation, endpoint and lease while its container stays offline. Raw ingress
  rejects browser HTTP/TLS before forwarding client bytes, preventing localhost
  cookies from reaching the tool. HTTP content uses the guarded preview path.
  See [port scope and regression checks](PORTS.md).

## Guidance is not enforcement

Instructions to avoid secrets, ignore prompt injection, prefer Rust, ask design
questions or write concise knowledge are model guidance. They do not establish a
security boundary. Enforcement comes from authenticated policy, capability
selection, scoped mounts, broker checks and publication fences. A malicious prompt
can still cause harmful changes inside an authorized writable workspace, misleading
feedback or disclosure through an available network tool. Source exclusions are
not universal secret detection, and independent judges do not prove correctness.

## Verification and remaining gaps

Test names above identify regression coverage, not proof against sandbox escapes.
`live_*` tests are explicit local checks, generally ignored in normal CI. Run the
pinned native and relevant container checks on supported architectures. Linux ARM
exposed incorrect numeric no-follow flags: native `libc` flags and
[platform-flags.rs](../tools/platform-flags.rs) now replace those constants.
The direct regression is `native_flags_reject_a_symlink_and_accept_its_regular_target`.
Related regressions include `publication_is_bounded_sorted_and_never_promotes_symlinks_or_private_files`
in [artifacts.rs](../src/artifacts.rs) and
`input_reader_rejects_symlinks_and_nonregular_files` in
[the browser tool](../tools/browser/main.rs). Passing macOS tests alone was
insufficient evidence for that boundary.

Before claiming complete web self-development readiness, verify the combined path:
real improvement snapshot → declared build job → delegation/judge → retained
results → draft publication/recovery. Component checks or a standalone source
archive build do not establish that this integrated path works. Record completed
live checks in the PR; this document is not a live-test completion report.

Remaining gaps are aggregate disk quotas/retention for workspaces and judge build
targets; server-side operator-session expiry/rotation (the browser cookie lifetime
does not expire its underlying bearer secret); API spend ceilings beyond usage
reporting and bounded task/delegation limits; and provider fault coverage for
older harnesses, credential expiry and interrupted session resume. Preserve
fail-closed behavior rather than restoring old workers or widening permissions
automatically. The local operator remains trusted to grant projects and review
external publication; approved source may contain sensitive material.
