# Windows command containment

`src/windows_launch.rs` is the native launch primitive used by the terminal
panel and bounded reads (review Git/checker, Work, Beads and toolchain probes).
Unix keeps its existing `Command`, `SharedChild` and process-group behavior.
The Engram runtime adapter still uses its existing transport; this change does
not migrate agent runtimes or make a claim about all processes on the machine.
All standard host command launches nevertheless share the Windows spawn lock
with the native primitive. Original pipe ends are non-inheritable; temporary
inheritable duplicates exist only inside that lock, and close before it is
released. The lock never covers a wait or output collection. A source check
rejects raw command/shared-child creation bypasses outside the bridge and its
explicitly guarded standard-library test references.

Each native launch records its process id and typed containment result in the
host diagnostics before the process resumes:

- `contained`: the root is confirmed in the host's kill-on-close Job Object,
  with the desktop-app policy that disables process-tree breakaway.
- `containedWithPackagedIdentityChanged`: the same arrangement, with the
  packaged root's full package name. Disabling breakaway makes descendants
  retain the package identity; this differs from normal packaged-desktop
  launch behavior and can affect their filesystem/environment view.
- `unavailable`: a reason identifies failed job setup/assignment, an
  unavailable desktop-app policy, or an unconfirmed identity/membership query.
  The command still runs. Any job with confirmed membership remains owned for cleanup, but
  the host does not claim containment. A process creation or pipe setup error
  still fails the command because there is no usable process to run.

The primitive uses `CreateProcessW` and an explicit handle list containing only
null stdin and the two output pipes. It creates a non-inheritable Job Object
with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, applies
`PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` with
`PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE`, and attempts
atomic `PROC_THREAD_ATTRIBUTE_JOB_LIST` assignment. If atomic creation is not
accepted, it replaces the refused job with a fresh lease, creates the process
suspended and assigns its original retained
process handle before resuming its original primary-thread handle. Pipe
readers attach before that resume. No PID/tree enumeration is used by this
primitive. The suspended-assignment fallback has a narrow crash window before
assignment: a host crash there can leave an unresumed process outside the job.

The command supervisor owns the sole job lease separately from waiters. TermAl
closes that lease when the command root exits, on Stop, or on a bounded-read
deadline. When TermAl exits, Windows closes its handles and ends remaining job
members. A restart therefore ends contained runs. Intermediate-only process
death does not itself close the root's lease or end the entire tree. Root death
requires the live host to observe that exit and close the lease; a Job Object
alone does not implement that policy.

Containment describes the established launch arrangement, not a sandbox or a
guarantee against every later executable. In particular, a descendant that
subsequently launches a Store-packaged executable can obtain a different
breakaway policy and escape; applying policy only to an outer ordinary Node
process does not solve that case. Directly launching Store PowerShell with the
policy is tested separately, including its changed descendant package identity.
Already-running services and processes outside the job are not owned.

The launch specification supports ordinary argv, inherited environment with
case-insensitive edits/removals, cwd and the creation flags used by these
callers. It is built where those choices are made. It does not reconstruct
opaque `Command` state, support `raw_arg`/`env_clear`, or directly execute
`.bat`/`.cmd` shims. Windows test-only `.ps1` read fixtures are explicitly
launched with PowerShell `-File`. The legacy `Command` fixture transport emits
`unavailable` because its opaque settings cannot establish the new contract.

Regression fixtures use IPC/file readiness receipts before capturing every
member's process handle and creation time. Assertions observe those handles
before any failure cleanup. They cover Node chains, root supervision of an
actual focused test launcher followed by interrupted-run recovery, exclusion
of unrelated inheritable handles, unavailable job setup, final exit code 259,
and byte/argv/environment/cwd parity with `Command`. Store PowerShell is either
tested as an installed package or explicitly reported not applicable because
that package is absent for the Windows test user.
Successful fixtures remove their owned scratch directory after process handles
are released, including copied executables. Failed fixtures retain it for
diagnosis. The real-launcher fixture uses a disposable repository so its
deliberately interrupted run never enters the host's review evidence store.

See the [test launcher guide](../test.md) and [architecture](../architecture.md).
