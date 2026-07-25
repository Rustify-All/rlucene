# Jenkins DeepSeek autofix prompt

You are fixing a Rust project failure reproduced by Jenkins.

1. Before reading any other repository file, read `AGENTS.md` completely and
   follow all applicable instructions in it.
2. Read `initial-test.log`, `initial-nextest-junit.xml` when present, and the
   relevant Rust source.
3. Before changing Rust code, locate and compare the corresponding Java
   implementation and Java test under the read-only `../java-reference`
   checkout. This checkout must be exactly
   `LuXugang/lucene:rlucene_10_1` at the commit recorded by Jenkins. Never use
   another Java branch and never modify the Java checkout. If no corresponding
   Java implementation or test exists, state that explicitly and continue only
   from reliable Rust evidence.
4. For a nextest timeout, use the exact test name, elapsed time, captured
   output, and random seed as the primary evidence.
5. Identify the root cause and make the smallest possible fix.
6. Do not run Cargo, tests, repository scripts, build scripts, or compiled code.
   Jenkins runs `cargo tidy`, formatting checks, nextest, and doctests after
   the DeepSeek credential is removed.
7. Do not add `#[ignore]`, delete tests, or weaken assertions.
8. Do not modify `.config/nextest.toml`, `AGENTS.md`, `Jenkinsfile`,
   `Jenkinsfile.autofix`, or `ci/jenkins/`.
9. Do not upgrade unrelated dependencies.
10. Do not commit, push, create a pull request, or merge.
11. If the evidence indicates infrastructure trouble or a flaky failure that
   should not be changed, leave the working tree unchanged and explain why.
