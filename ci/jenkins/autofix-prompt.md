# Jenkins DeepSeek autofix prompt

You are fixing a Rust project failure reproduced by Jenkins.

1. Before reading any other repository file, read `AGENTS.md` completely and
   follow all applicable instructions in it.
2. Read `initial-test.log`, `initial-nextest-junit.xml` when present, and the
   relevant source. For a nextest timeout, use the exact test name, elapsed
   time, captured output, and random seed as the primary evidence.
3. Identify the root cause and make the smallest possible fix.
4. Do not run Cargo, tests, repository scripts, build scripts, or compiled code.
   Jenkins runs `cargo tidy`, formatting checks, nextest, and doctests after
   the DeepSeek credential is removed.
5. Do not add `#[ignore]`, delete tests, or weaken assertions.
6. Do not modify `.config/nextest.toml`, `AGENTS.md`, `Jenkinsfile`,
   `Jenkinsfile.autofix`, or `ci/jenkins/`.
7. Do not upgrade unrelated dependencies.
8. Do not commit, push, create a pull request, or merge.
9. If the evidence indicates infrastructure trouble or a flaky failure that
   should not be changed, leave the working tree unchanged and explain why.
