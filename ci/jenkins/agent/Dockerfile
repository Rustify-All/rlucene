FROM jenkins/inbound-agent:jdk21

USER root

ARG RUST_VERSION=1.97.0
ARG OPENCODE_VERSION=1.18.4
ARG OPENCODE_LINUX_AMD64_SHA256=bab463c3fb3224d388bb7cfad63f38703df9cf0be2cfd2ce8cb49d886b53a174
ARG OPENCODE_LINUX_ARM64_SHA256=eba87efba3976d533a24cca0316f8ef375b5f8e797c0a95c25ee919700b7ba35
ARG TARGETARCH=amd64
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
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

RUN mkdir -p /home/jenkins/.local/bin /home/jenkins/.local/share/opencode \
    && case "${TARGETARCH}" in \
      amd64) \
        archive=opencode-linux-x64.tar.gz; \
        checksum="${OPENCODE_LINUX_AMD64_SHA256}" ;; \
      arm64) \
        archive=opencode-linux-arm64.tar.gz; \
        checksum="${OPENCODE_LINUX_ARM64_SHA256}" ;; \
      *) echo "Unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && curl -fsSL \
      "https://github.com/anomalyco/opencode/releases/download/v${OPENCODE_VERSION}/${archive}" \
      -o "/tmp/${archive}" \
    && printf '%s  %s\n' "${checksum}" "/tmp/${archive}" | sha256sum -c - \
    && tar -xzf "/tmp/${archive}" -C /home/jenkins/.local/bin \
    && opencode --version \
    && rm -f "/tmp/${archive}" \
    && chown -R jenkins:jenkins \
      /home/jenkins/.local \
      /opt/cargo \
      /opt/rustup

USER jenkins
