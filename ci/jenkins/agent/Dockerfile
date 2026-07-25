FROM jenkins/inbound-agent:jdk21

USER root

ARG RUST_VERSION=1.97.0
ARG NEXTEST_VERSION=0.9.140
ARG NEXTEST_LINUX_AMD64_SHA256=4ee9aaa0d0171a985a5d0eb735b87355894c1c455972e9674fb9fdbd1387c9a3
ARG NEXTEST_LINUX_ARM64_SHA256=8b3f4d4560b6b0f83774fecc6be07e47716dbad0eb0bb6c3890f478f4affe4b6
ARG OPENCODE_VERSION=1.18.4
ARG OPENCODE_LINUX_AMD64_SHA512=982899b8a401a911d586da76d181437cd7f9714c13b7afc8f7c3052500c97434d68209e40153a78ad413992b76710c283c9f7856b4700e1672637a48637f8242
ARG OPENCODE_LINUX_ARM64_SHA512=de0070f70784efa8e5a0d3c698fbf397718947c6fab1cc1200437150a45d277582902eea723266e3d285c9fa858cee7efb51f54368db9aaab1020b79ba9aa831
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

RUN case "${TARGETARCH}" in \
      amd64) \
        nextest_target=x86_64-unknown-linux-gnu; \
        nextest_checksum="${NEXTEST_LINUX_AMD64_SHA256}" ;; \
      arm64) \
        nextest_target=aarch64-unknown-linux-gnu; \
        nextest_checksum="${NEXTEST_LINUX_ARM64_SHA256}" ;; \
      *) echo "Unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && nextest_archive="cargo-nextest-${NEXTEST_VERSION}-${nextest_target}.tar.gz" \
    && curl -fsSL \
      "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/${nextest_archive}" \
      -o "/tmp/${nextest_archive}" \
    && printf '%s  %s\n' \
      "${nextest_checksum}" "/tmp/${nextest_archive}" | sha256sum -c - \
    && tar -xzf "/tmp/${nextest_archive}" -C /opt/cargo/bin cargo-nextest \
    && chmod 0755 /opt/cargo/bin/cargo-nextest \
    && cargo nextest --version \
    && rm -f "/tmp/${nextest_archive}"

RUN mkdir -p /home/jenkins/.local/bin /home/jenkins/.local/share/opencode \
    && case "${TARGETARCH}" in \
      amd64) \
        package=opencode-linux-x64; \
        checksum="${OPENCODE_LINUX_AMD64_SHA512}" ;; \
      arm64) \
        package=opencode-linux-arm64; \
        checksum="${OPENCODE_LINUX_ARM64_SHA512}" ;; \
      *) echo "Unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && archive="${package}-${OPENCODE_VERSION}.tgz" \
    && curl -fsSL \
      "https://registry.npmjs.org/${package}/-/${archive}" \
      -o "/tmp/${archive}" \
    && printf '%s  %s\n' "${checksum}" "/tmp/${archive}" | sha512sum -c - \
    && tar -xzf "/tmp/${archive}" -C /tmp \
    && cp /tmp/package/bin/opencode /home/jenkins/.local/bin/opencode \
    && chmod 0755 /home/jenkins/.local/bin/opencode \
    && opencode --version \
    && rm -rf "/tmp/${archive}" /tmp/package \
    && chown -R jenkins:jenkins \
      /home/jenkins/.local \
      /opt/cargo \
      /opt/rustup

USER jenkins
