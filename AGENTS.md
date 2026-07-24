# Repository instructions

## Project

This is a Rust workspace. Prefer small, root-cause fixes and preserve the
existing public API unless the task explicitly requires an API change.

## Validation

For normal interactive work, run:

- `cargo fmt --all -- --check`
- the narrowest relevant test
- `cargo test -q`

When `JENKINS_AUTOFIX=1`, do not execute Cargo, repository scripts, build
scripts, or tests. Jenkins removes the DeepSeek credential and performs all
validation after the coding agent exits.

## Restrictions

- Do not add `#[ignore]` to make a failure disappear.
- Do not delete tests or weaken assertions.
- Do not change Jenkins automation files as part of an application-code fix.
- Do not upgrade dependencies unless the failure is caused by the dependency.
- Do not commit, push, create pull requests, or merge branches.
- Do not access credentials or files outside the checked-out workspace.
