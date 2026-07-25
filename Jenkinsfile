pipeline {
  agent any

  options {
    skipDefaultCheckout(true)
    timestamps()
    disableConcurrentBuilds()
    timeout(time: 30, unit: 'MINUTES')
    buildDiscarder(logRotator(numToKeepStr: '100'))
  }

  triggers {
    cron('H/2 * * * *')
  }

  environment {
    REPOSITORY_URL = 'git@github.com:Rustify-All/rlucene.git'
    GIT_CREDENTIALS_ID = 'github-ssh'
    RUSTUP_DIST_SERVER = 'https://rsproxy.cn'
    RUSTUP_UPDATE_ROOT = 'https://rsproxy.cn/rustup'
    RUSTUP_HOME = '/opt/rustup'
    CARGO_HOME = '/opt/cargo'
    CARGO_TARGET_DIR = '/var/jenkins_home/cargo-target/rlucene-ci'
    CI_STATE_ROOT = '/var/jenkins_home/ci-state/rlucene-ci'
    LAST_SUCCESSFUL_SHA_FILE = '/var/jenkins_home/ci-state/rlucene-ci/last-successful-sha'
    CARGO_PROFILE_TEST_DEBUG = '0'
    CARGO_TERM_COLOR = 'never'
    RUST_BACKTRACE = 'full'
    NO_COLOR = '1'
    CARGO_NET_RETRY = '10'
    CARGO_HTTP_TIMEOUT = '120'
    CARGO_HTTP_MULTIPLEXING = 'false'
    SLOW_TEST_DIAGNOSTIC_AFTER_SECONDS = '300'
  }

  stages {
    stage('Initialize') {
      steps {
        script {
          env.FAILURE_KIND = 'none'
          env.FAILED_SHA = ''
          env.SKIP_PREFLIGHT = 'false'
        }
      }
    }

    stage('Storage status before build') {
      steps {
        sh '''#!/bin/bash
          set -u
          mkdir -p "$CARGO_TARGET_DIR"
          printf 'Jenkins storage before build:\\n'
          df -h /var/jenkins_home
          printf 'Temporary storage before build:\\n'
          df -h /tmp
          printf 'Cargo target before build:\\n'
          du -sh "$CARGO_TARGET_DIR"
        '''
      }
    }

    stage('Checkout main') {
      steps {
        checkout([
          $class: 'GitSCM',
          branches: [[name: '*/main']],
          extensions: [
            [
              $class: 'CleanBeforeCheckout',
              deleteUntrackedNestedRepositories: true
            ],
            [
              $class: 'CloneOption',
              depth: 1,
              honorRefspec: true,
              noTags: true,
              reference: '',
              shallow: true,
              timeout: 10
            ]
          ],
          userRemoteConfigs: [[
            credentialsId: env.GIT_CREDENTIALS_ID,
            name: 'origin',
            refspec:
              '+refs/heads/main:refs/remotes/origin/main',
            url: env.REPOSITORY_URL
          ]]
        ])
        script {
          String checkedOutSha = sh(
            returnStdout: true,
            script: 'git rev-parse HEAD'
          ).trim()
          if (!checkedOutSha) {
            error('Unable to determine the checked-out commit SHA')
          }
          env.FAILED_SHA = checkedOutSha
          currentBuild.description = checkedOutSha.take(12)

          int alreadyTested = sh(
            returnStatus: true,
            script: '''#!/bin/bash
              set -euo pipefail
              test -f "$LAST_SUCCESSFUL_SHA_FILE"
              test "$(cat "$LAST_SUCCESSFUL_SHA_FILE")" = "$FAILED_SHA"
            '''
          )
          if (alreadyTested == 0) {
            env.SKIP_PREFLIGHT = 'true'
            currentBuild.description =
              "${checkedOutSha.take(12)}: unchanged, direct nextest"
            echo """${checkedOutSha} already passed once.
Skipping dependency preflight and running cargo nextest directly."""
          }
        }
      }
    }

    stage('Infrastructure preflight') {
      when {
        expression { env.SKIP_PREFLIGHT != 'true' }
      }
      steps {
        sh '''#!/bin/bash
          set -euo pipefail
          mkdir -p "$CARGO_TARGET_DIR"
          df -h . /tmp "$CARGO_TARGET_DIR"
          git diff --exit-code
          rustup show
          cargo fetch
          cargo nextest --version
        '''
      }
    }

    stage('Cargo nextest') {
      steps {
        script {
          int testStatus = sh(
            returnStatus: true,
            script: '''#!/bin/bash
              set -uo pipefail
              rm -f \
                nextest.log \
                nextest-junit.xml \
                nextest-diagnostics.log
              rm -f target/nextest/ci/junit.xml
              if ! cargo nextest --version > nextest.log 2>&1; then
                cat nextest.log
                exit 125
              fi
              : > nextest-diagnostics.log

              stop_diagnostics() {
                if [ -n "${diagnostics_pid:-}" ]; then
                  kill "$diagnostics_pid" >/dev/null 2>&1 || true
                  wait "$diagnostics_pid" >/dev/null 2>&1 || true
                fi
              }
              trap stop_diagnostics EXIT

              set +e
              timeout --kill-after=30s 20m \
                cargo nextest run --profile ci --workspace \
                >> nextest.log 2>&1 &
              nextest_launcher_pid=$!
              bash ci/jenkins/capture-slow-test-diagnostics.sh \
                "$nextest_launcher_pid" \
                "$SLOW_TEST_DIAGNOSTIC_AFTER_SECONDS" \
                nextest-diagnostics.log &
              diagnostics_pid=$!

              wait "$nextest_launcher_pid"
              test_status=$?
              stop_diagnostics
              diagnostics_pid=''
              trap - EXIT

              junit_source="target/nextest/ci/junit.xml"
              if [ -f "$junit_source" ]; then
                cp "$junit_source" nextest-junit.xml
              fi
              cat nextest.log
              exit "$test_status"
            '''
          )
          String nextestLog = readFile(file: 'nextest.log')
          String nextestJunit = fileExists('nextest-junit.xml') ?
            readFile(file: 'nextest-junit.xml') : ''
          boolean testTimedOut = nextestLog.contains('TIMEOUT [')
          boolean nextestReportedFailure =
            nextestLog.contains('FAIL [') ||
            testTimedOut ||
            nextestLog.contains('\nerror:') ||
            nextestLog.startsWith('error:') ||
            nextestLog.contains('\nerror[') ||
            nextestLog.startsWith('error[') ||
            nextestJunit.contains('<failure') ||
            nextestJunit.contains('<error')

          if (testStatus == 124 || testStatus == 137) {
            env.FAILURE_KIND = 'suite-timeout'
            error(
              "cargo nextest exceeded the suite timeout or was killed " +
              "(exit ${testStatus})"
            )
          }

          if (testStatus == 125) {
            env.FAILURE_KIND = 'infrastructure'
            error('cargo-nextest is unavailable in the Jenkins environment.')
          }

          if (testStatus != 0) {
            if (nextestReportedFailure) {
              env.FAILURE_KIND = testTimedOut ? 'test-timeout' : 'code'
            } else {
              env.FAILURE_KIND = 'infrastructure'
            }
            error("cargo nextest failed (exit ${testStatus})")
          }
        }
      }
    }

    stage('Cargo doctest') {
      steps {
        script {
          int doctestStatus = sh(
            returnStatus: true,
            script: '''#!/bin/bash
              set -uo pipefail
              set +e
              timeout --kill-after=30s 4m \
                cargo test --workspace --doc -q \
                > doctest.log 2>&1
              test_status=$?
              cat doctest.log
              exit "$test_status"
            '''
          )
          String doctestLog = readFile(file: 'doctest.log')
          boolean doctestReportedFailure =
            doctestLog.contains('test result: FAILED') ||
            doctestLog.contains('\nerror:') ||
            doctestLog.startsWith('error:') ||
            doctestLog.contains('\nerror[') ||
            doctestLog.startsWith('error[')

          if (doctestStatus == 124 || doctestStatus == 137) {
            env.FAILURE_KIND = 'doctest-timeout'
            error(
              "cargo test --doc timed out or was killed " +
              "(exit ${doctestStatus})"
            )
          }

          if (doctestStatus != 0 || doctestReportedFailure) {
            env.FAILURE_KIND = 'code'
            error("cargo test --doc failed (exit ${doctestStatus})")
          }
        }
      }
    }

    stage('Record successful commit') {
      steps {
        sh '''#!/bin/bash
          set -euo pipefail
          umask 077
          mkdir -p "$CI_STATE_ROOT"
          state_tmp="$LAST_SUCCESSFUL_SHA_FILE.tmp.$BUILD_NUMBER"
          printf '%s\n' "$FAILED_SHA" > "$state_tmp"
          mv "$state_tmp" "$LAST_SUCCESSFUL_SHA_FILE"
        '''
      }
    }
  }

  post {
    always {
      script {
        int storageStatus = sh(
          returnStatus: true,
          script: '''#!/bin/bash
            set -u
            printf 'Jenkins storage after build:\\n'
            df -h /var/jenkins_home
            printf 'Temporary storage after build:\\n'
            df -h /tmp
            printf 'Cargo target after build:\\n'
            if [ -d "$CARGO_TARGET_DIR" ]; then
              du -sh "$CARGO_TARGET_DIR"
            else
              printf '%s does not exist\\n' "$CARGO_TARGET_DIR"
            fi
          '''
        )
        if (storageStatus != 0) {
          echo "Unable to report storage status (exit ${storageStatus})."
        }
      }
      archiveArtifacts(
        artifacts:
          'nextest.log,nextest-junit.xml,nextest-diagnostics.log,doctest.log',
        allowEmptyArchive: true
      )
    }

    failure {
      script {
        emailext(
          to: 'luxugang@apache.org',
          subject: "[Jenkins] ${env.JOB_NAME} #${env.BUILD_NUMBER} failed",
          body: """Build: ${env.BUILD_URL}
Commit: ${env.FAILED_SHA}
Failure kind: ${env.FAILURE_KIND}
""",
          attachLog: true,
          compressLog: true,
          attachmentsPattern:
            'nextest.log,nextest-junit.xml,nextest-diagnostics.log,doctest.log'
        )
      }
    }
  }
}
