# Jenkins Codex autofix prompt

You are fixing a Rust project failure reproduced by Jenkins.

1. Read `initial-test.log` and the relevant source.
2. Identify the root cause and make the smallest possible fix.
3. Do not run Cargo, tests, repository scripts, build scripts, or compiled code.
   Jenkins performs validation after the OpenAI credential is removed.
4. Do not add `#[ignore]`, delete tests, or weaken assertions.
5. Do not modify `AGENTS.md`, `Jenkinsfile`, `Jenkinsfile.autofix`, or
   `ci/jenkins/`.
6. Do not upgrade unrelated dependencies.
7. Do not commit, push, create a pull request, or merge.
8. If the evidence indicates infrastructure trouble or a flaky failure that
   should not be changed, leave the working tree unchanged and explain why.

