# Jenkins Codex autofix deployment

This deployment intentionally separates four trust zones:

1. `rlucene-ci` checks out `main`, fetches dependencies, and runs tests without
   OpenAI or GitHub API credentials.
2. `rlucene-autofix` reproduces the exact failing commit before Codex runs.
3. Codex receives only the OpenAI credential and has no GitHub write
   credential. Repository code is not executed during this stage.
4. Jenkins removes the OpenAI credential, independently formats/tests the
   patch, then injects GitHub credentials only for push and Draft PR creation.

Draft PRs are never auto-merged.

## Jenkins prerequisites

- Jenkins 2.555.2 or newer.
- Pipeline, Git, Credentials Binding, SSH Credentials, Pipeline Build Step,
  and Email Extension plugins.
- The existing SSH credential ID `github-ssh`.
- Outbound HTTPS access to:
  - `api.openai.com:443`
  - `api.github.com:443`
  - `github.com:443`
  - `objects.githubusercontent.com:443`
  - `raw.githubusercontent.com:443`
- Codex CLI at `/var/jenkins_home/tools/codex/bin/codex`.

The Codex installer can be retrieved from the pinned OpenAI Codex repository
path used by the readiness job. The official installer validates release
checksums. Pin `CODEX_RELEASE` when upgrading production intentionally.

## Jenkins credentials

Create these as Jenkins **Secret text** credentials:

- ID `openai-api-key`: an OpenAI Platform project API key. A Codex desktop
  sign-in is not usable by Jenkins. Restrict the API project's budget and
  rotate this key independently of personal credentials.
- ID `github-autofix-token`: a fine-grained GitHub token scoped only to this
  repository, with `Pull requests: Read and write`. Branch push uses the
  repository's existing Jenkins SSH credential.

If your Jenkins instance uses different IDs, update both Jenkinsfiles before
enabling the jobs.

Never put secret values in a Jenkinsfile, Dockerfile, build parameter, email,
GitHub repository, or console log.

## Jobs

Create two Pipeline-from-SCM jobs:

- `rlucene-ci`, script path `Jenkinsfile`.
- `rlucene-autofix`, script path `Jenkinsfile.autofix`.

During review, point both jobs to the deployment branch. After the Draft PR is
reviewed and merged manually, change their Pipeline SCM branch to `*/main`.
Disable the old Freestyle `rlucene` timer only after `rlucene-ci` has passed.

## Loop prevention

- Only `main` is accepted as a base branch.
- The CI job never builds `codex/*` branches.
- Each commit gets one persistent attempt marker under
  `/var/jenkins_home/codex-autofix-state/rlucene/`.
- A remote `codex/jenkins-autofix-<12-char-sha>` branch prevents another
  attempt.
- A failure that cannot be reproduced produces no patch and no PR.
- A timeout or killed process is treated as infrastructure failure.
- Pull requests are always Draft and are never auto-merged.

To retry a commit intentionally, an administrator must remove only that
commit's marker file after reviewing the previous attempt. Never clear the
entire state directory.

## Network verification

Configure a trusted HTTPS proxy or firewall route for the Jenkins execution
environment, then verify:

```sh
curl -sS --connect-timeout 10 --max-time 20 \
  -o /dev/null -w '%{http_code}\n' \
  https://api.openai.com/v1/models
```

An unauthenticated `401` response proves that the network path and TLS
connection work. A timeout means Codex cannot run.

## Jenkins root URL

Set the Jenkins root URL to the stable address that users and email recipients
can reach. Do not commit private network addresses to the repository.

## Isolated agent

The preferred long-term deployment uses the image in `ci/jenkins/agent/`.
Do not mount `/var/run/docker.sock`, the controller's Jenkins home, host SSH
keys, or unrelated repositories into that agent.
