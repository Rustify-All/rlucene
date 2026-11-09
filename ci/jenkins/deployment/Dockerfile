ARG JENKINS_BASE_IMAGE=jenkins/jenkins:2.580.2-jdk21@sha256:74401de6a6f92e1afe1b02d1c8e3d0c33f263bfb5275dd8813f9150355f2367b
FROM ${JENKINS_BASE_IMAGE}

USER root

ARG DEBIAN_MIRROR=https://mirrors.tuna.tsinghua.edu.cn/debian
ARG DEBIAN_SECURITY_MIRROR=https://mirrors.tuna.tsinghua.edu.cn/debian-security
RUN sed -i "s#http://deb.debian.org/debian-security#${DEBIAN_SECURITY_MIRROR}#g; s#http://deb.debian.org/debian#${DEBIAN_MIRROR}#g" /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        curl git build-essential pkg-config libssl-dev openssh-client \
        ca-certificates elfutils gdb libcap2-bin \
    && setcap cap_sys_ptrace=eip /usr/bin/eu-stack \
    && rm -rf /var/lib/apt/lists/*

ARG RUSTUP_DIST_SERVER=https://rsproxy.cn
ARG RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
ARG CARGO_REGISTRY=sparse+https://rsproxy.cn/index/
ARG NEXTEST_VERSION=0.9.143
ENV RUSTUP_DIST_SERVER=${RUSTUP_DIST_SERVER}
ENV RUSTUP_UPDATE_ROOT=${RUSTUP_UPDATE_ROOT}
ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH

RUN mkdir -p /opt/rustup /opt/cargo /opt/rlucene-toolchain \
    && chown -R jenkins:jenkins /opt/rustup /opt/cargo /opt/rlucene-toolchain

USER jenkins

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain none \
    && rm /tmp/rustup-init.sh

RUN printf '[source.crates-io]\nreplace-with = "rsproxy"\n\n[source.rsproxy]\nregistry = "%s"\n' \
    "$CARGO_REGISTRY" > /opt/cargo/config.toml

# Use the repository's toolchain file, including all required components.
COPY --chown=jenkins:jenkins rust-toolchain.toml /opt/rlucene-toolchain/rust-toolchain.toml
RUN cd /opt/rlucene-toolchain \
    && rustup show \
    && toolchain="$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)" \
    && test -n "$toolchain" \
    && rustup default "$toolchain" \
    && cargo install cargo-nextest --version "=${NEXTEST_VERSION}" --locked --jobs 2

COPY --chown=jenkins:jenkins ci/jenkins/deployment/plugins.txt /usr/share/jenkins/ref/plugins.txt
RUN jenkins-plugin-cli --latest=false --plugin-file /usr/share/jenkins/ref/plugins.txt

COPY --chown=jenkins:jenkins ci/jenkins/deployment/init.groovy.d/ /usr/share/jenkins/ref/init.groovy.d/

# JSON inventory selection for manual nightly/monster jobs. Keep the small
# runtime layer after the expensive Rust installation to preserve build cache.
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends jq \
    && rm -rf /var/lib/apt/lists/*
USER jenkins
