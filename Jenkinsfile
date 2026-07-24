pipeline {
  agent any

  options {
    skipDefaultCheckout(true)
    timestamps()
    disableConcurrentBuilds()
    timeout(time: 20, unit: 'MINUTES')
    buildDiscarder(logRotator(numToKeepStr: '100'))
  }

  triggers {
    cron('H/2 * * * *')
  }

  environment {
    REPOSITORY_URL = 'git@github.com:Rustify-All/rlucene.git'
    GIT_CREDENTIALS_ID = 'github-ssh'
    CARGO_TEST_FAILED = 'false'
    FAILURE_KIND = 'none'
    FAILED_SHA = ''
    RUSTUP_DIST_SERVER = 'https://rsproxy.cn'
    RUSTUP_UPDATE_ROOT = 'https://rsproxy.cn/rustup'
    RUSTUP_HOME = '/opt/rustup'
    CARGO_HOME = '/opt/cargo'
    CARGO_TARGET_DIR = '/var/jenkins_home/cargo-target/rlucene-ci'
    CARGO_PROFILE_TEST_DEBUG = '0'
    CARGO_TERM_COLOR = 'never'
    NO_COLOR = '1'
    CARGO_NET_RETRY = '10'
    CARGO_HTTP_TIMEOUT = '120'
    CARGO_HTTP_MULTIPLEXING = 'false'
  }

  stages {
    stage('Checkout main') {
      steps {
        deleteDir()
        checkout([
          $class: 'GitSCM',
          branches: [[name: '*/main']],
          userRemoteConfigs: [[
            credentialsId: env.GIT_CREDENTIALS_ID,
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
        }
      }
    }

    stage('Infrastructure preflight') {
      steps {
        sh '''#!/bin/bash
          set -euo pipefail
          mkdir -p "$CARGO_TARGET_DIR"
          df -h . /tmp "$CARGO_TARGET_DIR"
          git diff --exit-code
          rustup show
          cargo fetch --locked
        '''
      }
    }

    stage('Cargo test') {
      steps {
        script {
          int testStatus = sh(
            returnStatus: true,
            script: '''#!/bin/bash
              set -o pipefail
              timeout --kill-after=30s 12m cargo test -q \
                2>&1 | tee cargo-test.log
            '''
          )

          if (testStatus == 124 || testStatus == 137) {
            env.FAILURE_KIND = 'timeout'
            error("cargo test timed out or was killed (exit ${testStatus})")
          }

          if (testStatus != 0) {
            env.CARGO_TEST_FAILED = 'true'
            env.FAILURE_KIND = 'code'
            error("cargo test failed (exit ${testStatus})")
          }
        }
      }
    }
  }

  post {
    always {
      archiveArtifacts(
        artifacts: 'cargo-test.log',
        allowEmptyArchive: true
      )
    }

    failure {
      script {
        if (env.CARGO_TEST_FAILED == 'true') {
          try {
            build(
              job: 'rlucene-autofix',
              wait: false,
              parameters: [
                string(name: 'FAILED_SHA', value: env.FAILED_SHA),
                string(name: 'BASE_BRANCH', value: 'main'),
                string(name: 'UPSTREAM_BUILD_URL', value: env.BUILD_URL)
              ]
            )
          } catch (triggerError) {
            echo "Unable to trigger rlucene-autofix: ${triggerError}"
          }
        } else {
          echo "Autofix not triggered because failure kind is ${env.FAILURE_KIND}."
        }

        emailext(
          to: 'luxugang@apache.org',
          subject: "[Jenkins] ${env.JOB_NAME} #${env.BUILD_NUMBER} failed",
          body: """Build: ${env.BUILD_URL}
Commit: ${env.FAILED_SHA}
Failure kind: ${env.FAILURE_KIND}
Autofix triggered: ${env.CARGO_TEST_FAILED}
""",
          attachLog: true,
          compressLog: true,
          attachmentsPattern: 'cargo-test.log'
        )
      }
    }
  }
}
