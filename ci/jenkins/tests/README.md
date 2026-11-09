# Global build-lock regression check

`global-build-lock-smoke.groovy` is an **isolated Jenkins init script**. Never
copy it into a production home or execute it in an operational Script Console.
It requires a fresh home under a `mktemp -d` directory named
`rlucene-global-lock-test.XXXXXX`, and refuses to run without the explicit
`RLUCENE_GLOBAL_LOCK_SMOKE=1` opt-in. Bind its HTTP listener to loopback only.
Do not copy credentials, secrets, job data, or production configuration into it.

Prerequisites: Jenkins 2.580.2 WAR, Java 21 or 25, all plugins pinned in
`../deployment/plugins.txt`, and that Jenkins distribution's remoting JAR.
Install the plugins into the fresh home's `plugins/` directory. For example,
use the official Jenkins plugin installation manager with `--latest=false`,
the repository plugin file, and the fresh plugin download directory. An exact
copy of already-installed matching plugin **binaries only** is also sufficient.

Run with the following environment variables:

- `JENKINS_HOME`: `<temporary-directory>/home` (initially no jobs).
- `RLUCENE_GLOBAL_LOCK_SMOKE`: `1`.
- `RLUCENE_LOCK_TEST_REPO`: absolute checkout path containing this change.
- `RLUCENE_LOCK_TEST_REMOTING`: absolute path to the distribution's remoting JAR.
- `RLUCENE_LOCK_TEST_BASELINE_HOOK`: an exported copy of
  `b6021df72:ci/jenkins/deployment/init.groovy.d/rlucene-ci-idle-gate.groovy.override`.

Copy the smoke script to the fresh `home/init.groovy.d/` directory, then launch
the WAR with `-Djenkins.install.runSetupWizard=false`, a bounded Java heap,
`--httpListenAddress=127.0.0.1` and an unused `--httpPort`. This setup is only for
the disposable loopback test server; retain normal security on deployments.

The script starts a real local remoting agent on a second node and runs:

1. Full Declarative validation of all four production Jenkinsfiles, covering
   all five jobs, including the commit/PR minimum-debuginfo environment. Heavy
   bodies are then replaced by small shell probes while retaining the actual
   lock, agent, environment, timeout, and post structure.
2. A deterministic baseline reproduction using the former idle dispatcher:
   CI is already working when a commit job on the other node attempts to work.
   Atomic directory creation detects the overlap; the baseline commit fails.
3. Five simultaneous contenders on two nodes. Exactly one executor works and
   four Pipelines wait for the lock. An atomic marker remains owned through
   post/cleanup, so any overlap causes a failure.
4. The same CI-first/commit-second scenario with the new wrappers, both passing.
5. CI detects an untested revision and enters a shortened grace window without
   holding an executor or lock. A later commit build finishes first; CI then
   acquires the lock and continues.
6. CI queues first behind a holder, then a later PR queues; the PR must acquire
   the lock before the lower-priority CI without interrupting the holder.
7. Controlled failure, execution timeout, holder cancellation and waiter
   cancellation. The next build must proceed and no lock/marker may remain.
8. Re-evaluation of the resource initializer while a holder and waiters exist,
   confirming it preserves ownership and queue state. This is an idempotence
   check, **not** a substitute for an actual controller-restart acceptance test.
9. A CI waiter spends longer waiting for the lock than its shortened execution
   timeout, then still finishes successfully after acquiring the resource.

Read `<temporary-directory>/result.log`; a successful run ends with `SUCCESS`.
Job consoles and the process-entry/post/exit trace are retained in the temporary
directory for inspection. The script disables its test jobs and terminates its
local agent when finished; stop the test controller process yourself afterward.
No repository checkout, Cargo invocation, mail, or production job is performed.

These checks validate scheduling and cleanup, not Linux resource limits or
single-build peak memory. Repeat the deployment acceptance checks on the real
nodes in a maintenance window before restoring production triggers.
