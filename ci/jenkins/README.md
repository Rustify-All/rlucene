# Jenkins DeepSeek autofix deployment

This deployment intentionally separates four trust zones:

1. `rlucene-ci` checks out `main`, fetches dependencies, and runs tests without
   DeepSeek or GitHub API credentials.
2. `rlucene-autofix` reproduces the exact failing commit before OpenCode runs.
3. OpenCode uses DeepSeek and receives only the DeepSeek credential. It has no
   shell permission, no external-directory permission, and no GitHub write
   credential. Repository code is not executed during this stage.
4. Jenkins removes the DeepSeek credential, independently runs `cargo tidy`,
   formatting checks, and tests, then injects GitHub credentials only for push
   and ready-for-review PR creation.

Repair branches are pushed only to `LuXugang/rlucene`. Pull requests target
`Rustify-All/rlucene:main`, require human review, and are never auto-merged.

## Jenkins prerequisites

- Jenkins 2.555.2 or newer.
- Pipeline, Git, Credentials Binding, SSH Credentials, Pipeline Build Step,
  and Email Extension plugins.
- The existing SSH credential ID `github-ssh`.
- Outbound HTTPS access to:
  - `api.deepseek.com:443`
  - `api.github.com:443`
  - `github.com:443`
  - `objects.githubusercontent.com:443`
  - `raw.githubusercontent.com:443`
- OpenCode CLI at `/var/jenkins_home/tools/opencode/bin/opencode`.

DeepSeek exposes an OpenAI-compatible Chat Completions API. Codex CLI custom
providers currently require the Responses API, so this deployment uses
OpenCode as the restricted coding agent. The agent image pins the OpenCode
release and verifies the downloaded archive checksum.

## Jenkins credentials

Create these as Jenkins **Secret text** credentials:

- ID `deepseek-api-key`: a DeepSeek Platform API key. Restrict the account
  balance and rotate this key independently of personal credentials.
- ID `github-autofix-token`: because the head repository belongs to
  `LuXugang` while the base repository belongs to `Rustify-All`, use a classic
  PAT owned by `LuXugang` with the `public_repo` scope. Do not grant the broader
  `repo` scope while both repositories are public. A fine-grained PAT scoped
  only to the upstream repository cannot read the fork's head ref and GitHub
  rejects PR creation with `not all refs are readable`.

The `github-ssh` credential must be able to read `Rustify-All/rlucene` and push
branches to `LuXugang/rlucene`. The GitHub API token is not exposed until after
DeepSeek has exited and all independent validation has passed.

For a longer-lived production setup, replace the classic PAT with a dedicated
OAuth or GitHub App user token that has equivalent access to both repositories.

If your Jenkins instance uses different IDs, update both Jenkinsfiles before
enabling the jobs.

Never put secret values in a Jenkinsfile, Dockerfile, build parameter, email,
GitHub repository, or console log.

## Jobs

Create two Pipeline-from-SCM jobs:

- `rlucene-ci`, script path `Jenkinsfile`.
- `rlucene-autofix`, script path `Jenkinsfile.autofix`.

During review, point `rlucene-autofix` to the deployment branch in
`LuXugang/rlucene`. After the deployment PR is reviewed and merged manually,
change its Pipeline SCM repository to `Rustify-All/rlucene` and its branch to
`*/main`. The CI job continues to read the upstream repository.
Disable the old Freestyle `rlucene` timer only after `rlucene-ci` has passed.

## Repository instructions

The repository-root `AGENTS.md` is synchronized from the Codex workspace
instructions. The repair agent is explicitly required to read the entire file
before any other repository file, and the checkout fails if the file is
missing. Keep the repository copy synchronized whenever those principles
change. Jenkins blocks an autofix patch that modifies `AGENTS.md` or the
Jenkins deployment itself.

## Loop prevention

- Only `main` is accepted as a base branch.
- The CI job checks out only `main`; it never builds repair branches.
- Each commit gets one persistent attempt marker under
  `/var/jenkins_home/ai-autofix-state/rlucene/`.
- A remote `deepseek/jenkins-autofix-<12-char-sha>` branch in
  `LuXugang/rlucene` prevents another attempt.
- A failure that cannot be reproduced produces no patch and no PR.
- A timeout or killed process is treated as infrastructure failure.
- Pull requests are ready for review but are never auto-merged.

To retry a commit intentionally, an administrator must remove only that
commit's marker file after reviewing the previous attempt. Never clear the
entire state directory.

## Network verification

Configure a trusted HTTPS proxy or firewall route for the Jenkins execution
environment. When a proxy is required, add a Jenkins global environment
variable named `CODEX_HTTPS_PROXY`; this legacy variable name is retained to
avoid changing the already configured proxy. The autofix pipeline maps it to
the standard proxy variables only during DeepSeek connectivity and execution
stages.
Then verify:

```sh
curl -sS --connect-timeout 10 --max-time 20 \
  -o /dev/null -w '%{http_code}\n' \
  https://api.deepseek.com/v1/models
```

An unauthenticated `401` response proves that the network path and TLS
connection work. A timeout means the DeepSeek repair agent cannot run.

## Jenkins root URL

Set the Jenkins root URL to the stable address that users and email recipients
can reach. Do not commit private network addresses to the repository.

## Isolated agent

The preferred long-term deployment uses the image in `ci/jenkins/agent/`.
Do not mount `/var/run/docker.sock`, the controller's Jenkins home, host SSH
keys, or unrelated repositories into that agent.
