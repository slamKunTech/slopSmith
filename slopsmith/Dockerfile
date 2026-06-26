# ── Stage 1: Build RsCli ─────────────────────────────────────────────────
# TARGETARCH matches the final image (arm64 on Apple Silicon, amd64 on Intel/x86 servers).
# RsCli must match that arch: linux-x64 binaries do not run on linux/arm64.
FROM python:3.12-slim AS builder
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

# ── Stage 2: Final image ────────────────────────────────────────────────
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

# vgmstream-cli
RUN curl -sL https://github.com/vgmstream/vgmstream/releases/download/r2083/vgmstream-linux-cli.zip -o /tmp/vgm.zip \
    && unzip -o /tmp/vgm.zip -d /usr/local/bin/ \
    && chmod +x /usr/local/bin/vgmstream-cli \
    && rm /tmp/vgm.zip

# Copy RsCli from builder (no .NET SDK in final image)
COPY --from=builder /opt/rscli /opt/rscli

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY lib/ /app/lib/
COPY static/ /app/static/
COPY plugins/ /app/plugins/
COPY server.py /app/
COPY VERSION /app/

ENV PYTHONPATH=/app/lib:/app
ENV RSCLI_PATH=/opt/rscli/RsCli
ENV DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1

EXPOSE 8000

CMD uvicorn server:app --host 0.0.0.0 --port 8000
