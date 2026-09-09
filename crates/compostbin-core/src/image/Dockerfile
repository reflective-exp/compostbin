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

# The guest half of the host-command channel. It speaks the spool protocol in
# compostbin-core's `host` module, and is a shell script because the container is
# Linux while compostbin itself is a macOS binary.
COPY compostbin-host /usr/local/bin/compostbin-host
RUN chmod +x /usr/local/bin/compostbin-host

# Everything past this point belongs to the unprivileged user the session runs
# as. Nothing in a session should need root: the mounts are the user's own files,
# and a container that installs packages at run time is a container whose image
# is wrong.
RUN useradd --create-home --shell /bin/bash claude

# Both are mounted from the host at run time (D5, D2); created here, owned by
# `claude`, so the first run has a mountpoint even before the host directory
# exists and nothing under them is left owned by root.
RUN mkdir -p /home/claude/.claude /workspace \
 && chown -R claude:claude /home/claude /workspace

# Keeps `.claude.json` — the account and the onboarding answers — inside the
# mount. Without it Claude writes to ~/.claude.json, which dies with the
# container, and every restart asks for a fresh login.
ENV CLAUDE_CONFIG_DIR=/home/claude/.claude

USER claude
WORKDIR /workspace
