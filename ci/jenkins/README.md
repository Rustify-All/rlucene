# Jenkins CI deployment

The `rlucene-ci` Pipeline tests only `Rustify-All/rlucene:main`. Its repository
configuration is stored in `Jenkinsfile`; the old Freestyle job is kept
disabled as `legency` so its historical build records remain available.

## Jenkins prerequisites

- Jenkins 2.555.2 or newer.
- Pipeline, Git, SSH Credentials, and Email Extension plugins.
- The SSH credential ID `github-ssh`, with read access to
  `Rustify-All/rlucene`.
- Rust 1.97.0 with `rustfmt`, `clippy`, and `cargo-nextest` 0.9.140 available
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

## Checkout and caching

The Pipeline uses a main-only refspec, no tags, and a shallow checkout. Jenkins
keeps the workspace Git metadata between builds, so the first build clones the
repository and later builds normally fetch only changes to `main`.

The most recent successfully tested SHA is stored at
`/var/jenkins_home/ci-state/rlucene-ci/last-successful-sha`. If the current SHA
is unchanged, Jenkins skips dependency and infrastructure preflight, but still
runs nextest and doctests.

The persistent Cargo target is
`/var/jenkins_home/cargo-target/rlucene-ci`. Do not run `cargo clean` on every
scheduled build. Before and after every build, Jenkins logs free space for
Jenkins home and `/tmp`, plus the target directory size.

Rust test temporary files are isolated per build under
`$WORKSPACE_TMP/rlucene-ci-build-tmp/$BUILD_NUMBER` by setting `TMPDIR`. The
Pipeline removes stale data under its dedicated temporary root before a build
and deletes the current build directory from `post { always { ... } }`, so the
cleanup also runs after test failures and timeouts. This is safe because the
Pipeline disables concurrent builds. Do not clean the controller's global
`/tmp` from a scheduled build.

## Tests, timeouts, and diagnostics

The main test command is:

```sh
cargo nextest run --profile ci --workspace
```

Because nextest does not run Rust doctests, Jenkins also runs:

```sh
cargo test --workspace --doc -q
```

`.config/nextest.toml` marks an individual test as slow after 60 seconds. A slow
status is a warning and does not fail the build if the test eventually passes.
After 300 seconds, the Pipeline asks nextest to report all running test process
IDs, elapsed times, and captured output. It also writes system load, the process
tree, per-thread `/proc` state, kernel wait stacks, and a best-effort userspace
backtrace from `eu-stack`, `gdb`, or `pstack` to
`nextest-diagnostics.log`. If no debugger is installed or Linux ptrace policy
blocks attachment, the nextest status, process tree, resource usage, and
readable `/proc` diagnostics are still preserved.

The diagnostics helper recognizes a nextest test process by the `--exact`
argument used for a test-harness invocation. Cargo, Git, and other child
processes used while nextest is resolving dependencies are not treated as slow
tests and never cause the helper to send `SIGUSR1` to nextest.

The deployed container configuration and its verification procedure are
documented in `ci/jenkins/deployment/README.md`.

An individual test is terminated and reported as `TIMEOUT` only after 360
seconds. Nextest first sends `SIGTERM`, waits for a 30-second grace period, and
then sends `SIGKILL` if necessary. The Pipeline adds a 20-minute outer timeout
for the complete nextest run, a 4-minute timeout for doctests, and a 30-minute
timeout for the complete build.

Jenkins archives `nextest.log`, `nextest-junit.xml`,
`nextest-diagnostics.log`, and `doctest.log`. Failure emails include the commit
SHA, failure classification, compressed console log, and the available
diagnostic artifacts. Failures are reported for human investigation; this
repository no longer starts an automatic repair job.

## Jenkins root URL

Set the Jenkins root URL in Jenkins administration to a stable address that
users and email recipients can reach. Do not commit private network addresses
to the repository.
