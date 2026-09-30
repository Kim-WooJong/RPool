FROM debian:bookworm
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates curl build-essential pkg-config git fuse3 libfuse3-dev \
    libxkbcommon-dev libwayland-dev libudev-dev libgtk-3-dev && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain nightly --component rustfmt,clippy
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /src
