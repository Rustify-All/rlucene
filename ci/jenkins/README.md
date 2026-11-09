# Jenkins CI deployment

The `rlucene-ci` Pipeline defaults to `Rustify-All/rlucene:main`. Its definition
is [Jenkinsfile](Jenkinsfile) in this directory. The controller image, pinned
plugins, Compose service, and automatic job creation are versioned under
[deployment](deployment/README.md). Follow that guide to start a new instance
from a clone of this repository or configure it to build your fork.

The job checks out the same repository and branch that supplied its Jenkinsfile.
Scheduled and manual release jobs have their own build cache and state. The
commit and PR checks use per-build Cargo targets that are deleted after each
run. The original controller's disabled `legency` job and historical builds
are instance data; new installations create only `rlucene-ci`.

## Pull request commit-check job

The `Jenkins PR test-light` GitHub Actions workflow handles pull request events.
It asks GitHub for the PR author's effective repository permission and starts
Jenkins only for `write` or `admin`. An external contributor's workflow exits
successfully without contacting Jenkins.

Authorized requests start the separate `rlucene-pr` Pipeline. The Pipeline
validates every parameter, fetches `refs/pull/<number>/head`, verifies that the
checked-out commit is the exact SHA reported by GitHub, and runs:

```sh
cargo commit
```

The GitHub Actions run waits for Jenkins. Jenkins `SUCCESS` becomes a green
GitHub check; any other terminal result or timeout becomes a failed check. The
Actions summary links to the Jenkins build. A superseding update to the same PR
cancels the older Actions run and asks Jenkins to stop its obsolete build.
If the obsolete request is still queued, it cancels the queue item and checks
whether a build started during that handoff. Different PRs do not cancel each
other; cancelled build records remain retained.
Every Jenkins PR build is protected from automatic deletion, including its
Console Output and build status. The Pipeline does not archive a duplicate
Cargo log. The normal 200-build history limit applies only to a run that fails
before the Pipeline can mark it for retention.

PR code runs only on the exclusive `rlucene-pr` inbound agent. That container
does not mount Jenkins home or the Docker socket. The existing scheduled
`rlucene-ci` job remains on the built-in node and is unchanged.

## Main branch commit-check job

The `Jenkins main test-light` workflow starts `rlucene-commit` once for each
push to `Rustify-All/rlucene:main`, including merge, squash and rebase merges
and direct pushes. It tests the final SHA of that push, not every intermediate
commit in a multi-commit push. Closing an unmerged PR does not trigger it.
The workflow is restricted to the upstream repository so pushing the same
branch to a fork does not create a duplicate Jenkins build.

The Pipeline fetches the requested SHA directly, verifies the checkout, and
runs `cargo commit`. This performs `cargo tidy`, verifies that those checks did
not change the checkout, and then runs the fast test suite. Later main updates
do not cancel earlier runs or change the revision being tested. Each push has
its own GitHub check and Jenkins build link. The workflow allows 40 minutes to start a build and
35 minutes for it to finish; queue or build timeouts fail the GitHub check.

`rlucene-commit` shares the existing single-executor `rlucene-pr` agent with
PR testing. Each job has a separate workspace and per-build Cargo target; their
tests run serially on that agent. Before a build starts, the Pipeline removes
any target left by an interrupted earlier run. Its `post` cleanup removes the
current target after success, failure, or cancellation. `/opt/cargo` remains
available as the dependency download cache, while compiler output is not kept
between revisions. All commit builds are retained, with no age or count limit.
The scheduled `rlucene-ci` release job remains separate.

Both commit-check Pipelines set `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_PROFILE_TEST_DEBUG=0`. This removes compiler debuginfo from the xtask,
tidy and test artifacts to reduce temporary disk and memory pressure. It does
not disable debug assertions or overflow checks, so test behavior remains the
normal non-release behavior.

## Jenkins prerequisites

- Jenkins 2.580.2 with Java 21, as pinned in the controller image.
- The plugin versions in `deployment/plugins.txt`, installed by the image.
- No Git credential is needed for the scheduled job's default public HTTPS
  checkout. The member-only PR job is intentionally fixed to
  `git@github.com:Rustify-All/rlucene.git` and requires the existing Jenkins SSH
  credential ID `github-ssh`; Jenkins uses it for the trusted definition checkout
  and the exact PR-ref checkout on the agent.
- Rust 1.98.0 with `rustfmt`, `clippy`, and `cargo-nextest` 0.9.143 available
  through `/opt/cargo/bin`.
- The version-controlled controller image and Compose configuration under
  `ci/jenkins/deployment`. They install `eu-stack` and `gdb` and grant the
  minimum ptrace capability needed by the dedicated `eu-stack` executable. The
  image configures Cargo to use the rsproxy sparse registry. It also installs
  the Simple Theme plugin and applies the version-controlled classic Pipeline
  console theme from `ci/jenkins/deployment/init.groovy.d`.
- Outbound access to GitHub and the configured Rust package mirrors.

Never put credential values in a Jenkinsfile, build parameter, email,
repository file, or console log.

## Scheduling

Run the new job once using **Build Now**. That run installs the Pipeline's
schedule and options: every minute (`* * * * *`), no concurrent builds,
500 retained builds, a 30-second new-revision grace window, and a 45-minute
execution-stage timeout. The schedule is defined directly in `Jenkinsfile`.
After a schedule change is pushed, the next build loads the updated file and
applies it without restarting the controller.
The former `RLUCENE_CRON` environment setting is no longer used.

All five managed jobs (`rlucene-ci`, `rlucene-commit`, `rlucene-pr`,
`rlucene-nightly`, `rlucene-monster`) acquire the same Lockable Resources
resource, `rlucene-vm-build`. The top-level Pipeline has `agent none`; its
lock surrounds the resource-intensive execution stage, its node allocation,
environment, final checkout, tests, and stage `post` actions (including
archiving and cleanup). Only one job can execute that protected body across the
controller and PR agent. Existing per-job `disableConcurrentBuilds()` is
retained.

Before requesting the lock, `rlucene-ci` performs a lightweight checkout on the
controller and compares the observed `main` SHA with its last successful SHA.
If the revision has not passed CI, the Pipeline releases the executor and waits
30 seconds without holding the global lock. This gives the GitHub-triggered
`rlucene-commit` or `rlucene-pr` request time to reach Jenkins. After the grace
window CI requests the lock at its lower priority and refreshes the checkout
again, so a newer revision that arrived during the wait is not missed. An
already-successful unchanged revision skips the grace window.

`rlucene-ci` requests the lock with priority `-100`; the other four jobs use
the plugin default priority `0`. A commit, PR, nightly, or monster build that
arrives after CI has started waiting therefore moves ahead of that waiting CI.
Priority changes only the wait queue: it never interrupts a build that already
holds the lock. Equal-priority waiters retain their normal queue order. Under a
continuous stream of higher-priority work, CI waits until that work drains.

Waiting Pipelines can appear as running builds in Jenkins, but do not hold
an executor or start Cargo. Fetching the trusted Jenkinsfile and the lightweight
revision probe happen before the Pipeline lock; neither is a test build. The
execution timeout begins after acquiring the lock and includes waiting for the
selected node. Existing test subprocess timeouts are unchanged.
The lock selects the highest-priority waiter without interrupting the holder;
long manual suites therefore still delay automatic tests once they start.
Trigger policies and the CI one-minute schedule are unchanged. New jobs must
use the same wrapper before they are allowed to run resource-intensive work on
this VM.

The former CI-only queue gate is retired, and `RLUCENE_CI_REQUIRE_IDLE` is
no longer used. Do not combine it with lock waiters, which Jenkins already
considers running. See the deployment README for the required plugin and
maintenance-window rollout. Serialization prevents overlapping builds; it
does not cap a single build's memory or its internal test parallelism.

The scheduled release job sets `CARGO_BUILD_JOBS=4` to bound compiler
parallelism on the 30 GiB CI VM. This also bounds Rust compiler backend work
that shares Cargo's jobserver. Nextest test parallelism is configured separately
and remains unchanged. Release optimization settings are unchanged.

## Checkout and caching

The generated job uses a single-branch refspec, no tags, a depth-one clone,
and clean checkout. Jenkins keeps the workspace Git metadata between builds, so the first build clones the
repository and later builds normally fetch only changes to the configured branch.

The most recent successfully tested SHA is stored at
`$JENKINS_HOME/ci-state/$JOB_NAME/last-successful-sha` (the default job retains
the original `/var/jenkins_home/ci-state/rlucene-ci/last-successful-sha` path).
Before locking, this state controls the 30-second grace window. After locking,
Jenkins refreshes the checkout and checks the state again; if the current SHA is
unchanged, it skips dependency and infrastructure preflight, but still runs
nextest and doctests.

The persistent Cargo target is
`$JENKINS_HOME/cargo-target/$JOB_NAME`. Do not run `cargo clean` on every
scheduled build. Before and after every build, Jenkins logs free space for
Jenkins home and `/tmp`, plus the target directory size.

Before every nextest run, including an unchanged successful commit, the
Pipeline checks the `CARGO_MANIFEST_DIR` recorded in rlucene release dependency
files against the current checkout. If another source directory populated the
cache, it invalidates only rlucene's release artifacts with
`cargo clean --release -p rlucene`; dependency artifacts remain cached. Rust
tests embed their source directory when locating fixture files, so binaries
from a deleted diagnostic checkout must not be reused. The check is logged in
`release-cache-check.log` and runs under the existing global build lock.
When the commit and source directory are unchanged, the check preserves the
cache; it does not touch source timestamps or force recompilation.

Temporary diagnostics must use their own `CARGO_TARGET_DIR` and must not borrow
the persistent target of `rlucene-ci` or another job. Deleting a diagnostic
checkout is safe only when its target is also isolated from persistent jobs.

Rust test temporary files are isolated per build under
`$WORKSPACE_TMP/rlucene-ci-build-tmp/$BUILD_NUMBER` by setting `TMPDIR`. The
Pipeline removes stale data under its dedicated temporary root before a build
and deletes the current build directory from `post { always { ... } }`, so the
cleanup also runs after test failures and timeouts. This is safe because the
Pipeline disables concurrent builds. Do not clean the controller's global
`/tmp` from a scheduled build.

## Nightly and monster jobs

`rlucene-nightly` runs every two hours (`TZ=Asia/Shanghai`, `H */2 * * *`),
at a stable Jenkins-selected minute in each even-numbered hour. It also accepts
**Build Now** while signed in. `rlucene-monster` remains **manual only**.
Neither has SCM polling, webhook, or upstream triggers; the Pipeline accepts
user causes for both jobs and timer causes only for nightly. GitHub's
PR/commit service account does not receive build permission on these jobs.
Both use `main` and `ci/jenkins/manual/Jenkinsfile` from SCM, on the controller
like `rlucene-ci`. Scheduled nightly builds wait for the same global lock as all
other production jobs and, like every non-CI job, outrank a waiting `rlucene-ci`;
two hours is the trigger interval, not a guarantee of the actual start time.
Existing per-job non-concurrency and the 72-hour execution budget remain
unchanged.

- `rlucene-nightly`: only tests added by the `nightly` feature,
  with `tests.nightly=true` and `tests.light=false`.
- `rlucene-monster`: only tests added by the `monster` feature,
  with `tests.nightly=false` and `tests.light=false`; it does not enable nightly.
- Both use `--release` and the same release debug-assertion setting as
  `rlucene-ci`, not the Rust nightly toolchain or a custom Cargo build profile.

The helper compares full test inventories with and without the requested
feature, then selects **only tests added by that feature**. All baseline tests
are excluded, whether normally enabled or ignored. Feature-only heavy tests
run even if their ignore reason describes their resource requirements rather
than saying `nightly`/`monster`. Full binary IDs and test names are matched.
An empty feature-only selection fails explicitly; it never falls back to the
ordinary suite. These jobs do not run doctests; ordinary tests and doctests
remain covered by `rlucene-ci`. Compiling and listing the baseline tests is
still needed for selection, but does not execute their test bodies.

The two feature variants must be compiled on the first run. Each job has its
own persistent `cargo-target/${JOB_NAME}` cache, reused by subsequent builds;
neither clears or borrows the `rlucene-ci` target. The `manual` nextest profile
runs tests **one at a time**, warns after 60 seconds and terminates an individual
test after 12 hours (plus 30 seconds grace). The entire Pipeline has a 72-hour
safety limit. These jobs can use large amounts of RAM and disk; serial execution
does not guarantee that the server can accommodate every monster test.

Builds that reach the build-request stage are kept forever, including aborted
smoke builds. Logs, JUnit when available, and the exact selection filter are
archived. A new-server deployment smoke check should start each job in turn,
confirm `rustc` is compiling with `--cfg feature="nightly"` or `monster` and
release optimization, then **cancel immediately** and check its subprocesses
have exited. This validates startup/command wiring, not successful compilation,
test completion, or hardware capacity for the full heavy suites.

## Tests, timeouts, and diagnostics

The main release test command is run with debug assertions enabled:

```sh
CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
  cargo nextest run --release --profile ci --workspace
```

Jenkins sets `CARGO_TARGET_DIR` to the persistent
`/var/jenkins_home/cargo-target/rlucene-ci` directory. Builds must not run
unconditional `cargo clean` or otherwise remove this directory: Cargo keeps release
artifacts there and reuses them when the commit is unchanged or when source
dependencies are unchanged.

Because nextest does not run Rust doctests, Jenkins also runs:

```sh
CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
  cargo test --release --workspace --doc -q
```

The release-only `ci` profile in `.config/nextest.toml` marks an individual test
as slow after 30 seconds. A slow status is a warning and does not fail the build
if the test eventually passes. At 30 seconds, the Pipeline also asks nextest to
report all running test process IDs, elapsed times, and captured output. It
writes system load, the process tree, per-thread `/proc` state, kernel wait
stacks, and a best-effort userspace backtrace from `eu-stack`, `gdb`, or
`pstack` to `nextest-diagnostics.log`. If no debugger is installed or Linux
ptrace policy blocks attachment, the nextest status, process tree, resource
usage, and readable `/proc` diagnostics are still preserved.

The diagnostics helper recognizes a nextest test process by the `--exact`
argument used for a test-harness invocation. Cargo, Git, and other child
processes used while nextest is resolving dependencies are not treated as slow
tests and never cause the helper to send `SIGUSR1` to nextest.

The deployed container configuration and its verification procedure are
documented in `ci/jenkins/deployment/README.md`.

An individual Jenkins release test is terminated and reported as `TIMEOUT`
after 60 seconds. Nextest first sends `SIGTERM`, waits for a 30-second grace
period, and then sends `SIGKILL` if necessary. Non-release runs keep the default
60-second slow threshold and 360-second timeout. The Pipeline adds a 40-minute
outer timeout for the complete nextest build and test run, a 4-minute timeout
for doctests, and a 45-minute timeout for the execution stage, including checkout
and post actions. Waiting for the shared build lock is outside that budget.

Jenkins archives `nextest.log`, `nextest-junit.xml`,
`nextest-diagnostics.log`, and `doctest.log`. When `RLUCENE_FAILURE_EMAIL` is
configured and SMTP is set up in Jenkins,
code or test failure emails include the commit SHA, failure classification,
compressed console log, and the available diagnostic artifacts. Checkout,
network, environment preflight, and cleanup failures are treated as
infrastructure failures and do not send email. Email is disabled by default;
logs and diagnostics are always archived. Test failures are reported for human
investigation; this
repository no longer starts an automatic repair job.

## Jenkins root URL

Set the Jenkins root URL in Jenkins administration to a stable address that
users and email recipients can reach. Do not commit private network addresses
to the repository.
