# Jenkins controller container

These files are the version-controlled backup of the Jenkins controller
deployment. The deployed copies live together in `/home/xugang/jenkins` on the
Jenkins VM. Jenkins configuration, credentials, job history, and workspaces
remain in the external `jenkins_home` named volume and are not stored here.
The image uses the rsproxy sparse registry for Cargo dependencies.

No credential value or private key may be added to these files.

## Resource allocation

The Jenkins VM is dedicated to Jenkins, so the Compose service intentionally
has no `cpus` or `mem_limit` setting. The controller and its build processes can
use all CPU and memory assigned to the VM. `JAVA_OPTS` still caps the Jenkins
controller heap at 8 GiB; this does not limit separate Cargo and test
processes.

## Pipeline console theme

The image installs the Simple Theme plugin and configures a small inline theme
from `init.groovy.d/rlucene-console-theme.groovy.override`. Classic Pipeline
console pages hide implementation-only flow nodes such as `withEnv`,
`timestamps`, `timeout`, `script`, and `sh`, while labeled stage boundaries are
rendered as concise headings. The raw `consoleText` response is unchanged, so
downloaded logs, failure emails, and diagnostic data retain the complete
Pipeline metadata.

The `.override` suffix is intentional. The Jenkins Docker entrypoint copies the
script to the persistent `jenkins_home` volume on every container start, keeping
the repository version authoritative without clearing unrelated initialization
scripts.

## Deploy

Copy `Dockerfile`, `docker-compose.yml`, and `init.groovy.d` to
`/home/xugang/jenkins`, then run:

```sh
cd /home/xugang/jenkins
docker compose config --quiet
docker compose build jenkins
docker compose up -d --force-recreate jenkins
docker compose ps
```

Back up the currently deployed files before replacing them so configuration
rollback does not require rebuilding the previous revision from memory. The
named volume is retained when the container is recreated.

After deployment, open a completed Pipeline's classic Console Output and verify
that stage headings remain visible while the other `[Pipeline]` flow-node lines
are hidden. Also verify that the build's `consoleText` download still contains
`[Pipeline] Start of Pipeline`.

## Slow-test stack capture

The image installs `eu-stack` from `elfutils` and `gdb`. Jenkins runs as the
non-root `jenkins` user, so `/usr/bin/eu-stack` has the file capability
`cap_sys_ptrace=eip`; the Compose service adds `SYS_PTRACE` to the container
capability bounding set. This limits effective ptrace permission to the
dedicated stack-capture executable instead of granting it to the Jenkins Java
process.

Verify the deployed tools and capability with:

```sh
docker exec jenkins eu-stack --version
docker exec jenkins gdb --version
docker exec jenkins /usr/sbin/getcap /usr/bin/eu-stack
```

Expected capability output:

```text
/usr/bin/eu-stack cap_sys_ptrace=eip
```

Do not remove either the file capability or Compose `SYS_PTRACE`: both are
required for `ci/jenkins/capture-slow-test-diagnostics.sh` to attach to test
processes while Jenkins continues to run as a non-root user.
