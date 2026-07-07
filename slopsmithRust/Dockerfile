# ── Stage 1: Build RsCli ─────────────────────────────────────────────────
# TARGETARCH matches the final image (arm64 on Apple Silicon, amd64 on Intel/x86 servers).
# RsCli must match that arch: linux-x64 binaries do not run on linux/arm64.
FROM python:3.12-slim AS rscli-builder
ARG TARGETARCH

RUN apt-get update && apt-get install -y --no-install-recommends curl git && rm -rf /var/lib/apt/lists/*

RUN curl -sL https://dot.net/v1/dotnet-install.sh -o /tmp/dotnet-install.sh \
    && chmod +x /tmp/dotnet-install.sh \
    && /tmp/dotnet-install.sh --channel 10.0 --install-dir /usr/share/dotnet \
    && ln -s /usr/share/dotnet/dotnet /usr/local/bin/dotnet

ENV DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1
ENV DOTNET_CLI_TELEMETRY_OPTOUT=1

RUN git clone --depth 1 https://github.com/iminashi/Rocksmith2014.NET.git /tmp/rs2014

COPY rscli/RsCli.fsproj /tmp/rs2014/tools/RsCli/
COPY rscli/Program.fs /tmp/rs2014/tools/RsCli/

RUN sed -i 's|</PropertyGroup>|<NuGetAudit>false</NuGetAudit></PropertyGroup>|' /tmp/rs2014/Directory.Build.props \
    && cd /tmp/rs2014/tools/RsCli \
    && case "$TARGETARCH" in \
         arm64) RID=linux-arm64 ;; \
         amd64) RID=linux-x64 ;; \
         *) RID=linux-x64 ;; \
       esac \
    && dotnet publish -c Release -r "$RID" --self-contained -o /opt/rscli

# ── Stage 2: Build the Rust project ──────────────────────────────────────
FROM rust:1.79-slim AS rust-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation: copy the manifest first, build a stub, then
# copy the real sources and build the actual binaries.
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src/bin \
    && echo 'fn main() {}' > src/main.rs \
    && echo 'fn main() {}' > src/bin/psarc_to_sloppak.rs \
    && echo 'fn main() {}' > src/bin/split_stems.rs \
    && cargo build --release || true \
    && rm -rf src

COPY . .
RUN cargo build --release \
    && cargo build --release --bin psarc-to-sloppak --bin split-stems

# ── Stage 3: Final image ─────────────────────────────────────────────────
FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg \
    fluidsynth \
    fluid-soundfont-gm \
    libsndfile1 \
    curl \
    unzip \
    megatools \
    && rm -rf /var/lib/apt/lists/*

# vgmstream-cli (needed by psarc-to-sloppak to decode WEM audio)
RUN curl -sL https://github.com/vgmstream/vgmstream/releases/download/r2083/vgmstream-linux-cli.zip -o /tmp/vgm.zip \
    && unzip -o /tmp/vgm.zip -d /usr/local/bin/ \
    && chmod +x /usr/local/bin/vgmstream-cli \
    && rm /tmp/vgm.zip

# Demucs is an optional Python tool driven by the split-stems binary.
RUN pip install --no-cache-dir demucs

# Copy RsCli from its builder stage (no .NET SDK in the final image).
COPY --from=rscli-builder /opt/rscli /opt/rscli

WORKDIR /app

# Copy the compiled Rust binaries from the build stage.
COPY --from=rust-builder /build/target/release/slopsmith /usr/local/bin/slopsmith
COPY --from=rust-builder /build/target/release/psarc-to-sloppak /usr/local/bin/psarc-to-sloppak
COPY --from=rust-builder /build/target/release/split-stems /usr/local/bin/split-stems

# Static frontend assets + version metadata.
COPY static/ /app/static/
COPY VERSION /app/

ENV RSCLI_PATH=/opt/rscli/RsCli
ENV DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1

EXPOSE 8000

CMD ["slopsmith"]
