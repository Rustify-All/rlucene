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

## Tests, timeouts, and diagnostics

The main test command is:

```sh
cargo nextest run --profile ci --workspace
```

Because nextest does not run Rust doctests, Jenkins also runs:

```sh
cargo test --workspace --doc -q
```

`.config/nextest.toml` marks an individual test as slow after 60 seconds and
terminates it after about 120 seconds. The Pipeline adds a 12-minute outer
timeout for nextest, a 4-minute timeout for doctests, and a 20-minute timeout
for the complete build.

Jenkins archives `nextest.log`, `nextest-junit.xml`, and `doctest.log`. Failure
emails include the commit SHA, failure classification, compressed console log,
and the available diagnostic artifacts. Failures are reported for human
investigation; this repository no longer starts an automatic repair job.

## Jenkins root URL

Set the Jenkins root URL in Jenkins administration to a stable address that
users and email recipients can reach. Do not commit private network addresses
to the repository.
