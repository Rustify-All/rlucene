FROM jenkins/inbound-agent:jdk21

USER root

ARG RUST_VERSION=1.97.0
ARG CODEX_VERSION=0.145.0
ARG CODEX_INSTALLER_COMMIT=ef2d3edb959a75a90665ade37d554f3ca65fe880
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    CODEX_HOME=/home/jenkins/.codex \
    PATH=/home/jenkins/.local/bin:/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
      build-essential \
      ca-certificates \
      curl \
      git \
      libssl-dev \
      pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
      -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh \
      -y \
      --no-modify-path \
      --profile minimal \
      --default-toolchain "${RUST_VERSION}" \
    && rustup component add rustfmt clippy rust-src \
    && rm -f /tmp/rustup-init.sh

RUN mkdir -p /home/jenkins/.local/bin /home/jenkins/.codex \
    && curl -fsSL \
      "https://raw.githubusercontent.com/openai/codex/${CODEX_INSTALLER_COMMIT}/scripts/install/install.sh" \
      -o /tmp/install-codex.sh \
    && CODEX_RELEASE="${CODEX_VERSION}" \
      CODEX_INSTALL_DIR=/home/jenkins/.local/bin \
      CODEX_NON_INTERACTIVE=1 \
      sh /tmp/install-codex.sh \
    && codex --version \
    && rm -f /tmp/install-codex.sh \
    && chown -R jenkins:jenkins \
      /home/jenkins/.local \
      /home/jenkins/.codex \
      /opt/cargo \
      /opt/rustup

USER jenkins
