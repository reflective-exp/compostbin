FROM debian:stable-slim

RUN apt-get update \
 && apt-get install --no-install-recommends --yes \
      build-essential \
      ca-certificates \
      curl \
      git \
      jq \
      less \
      openssh-client \
      procps \
      ripgrep \
      unzip \
 && rm -rf /var/lib/apt/lists/*

# Debian stable's node is older than Claude Code supports.
RUN curl --fail --silent --show-error --location https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install --no-install-recommends --yes nodejs \
 && rm -rf /var/lib/apt/lists/*

RUN npm install --global @anthropic-ai/claude-code

# Mounted from the host at run time (D5); created here so the first run has a
# mountpoint even before the host directory exists.
RUN mkdir -p /root/.claude

WORKDIR /root
