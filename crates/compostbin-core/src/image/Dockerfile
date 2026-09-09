FROM debian:stable-slim

RUN apt-get update \
 && apt-get install --no-install-recommends --yes \
      build-essential \
      ca-certificates \
      curl \
      direnv \
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

# direnv only takes effect through a shell hook, and Claude Code's Bash tool
# initializes each shell from the profile, so hook it there.
RUN echo 'eval "$(direnv hook bash)"' >> /root/.bashrc

# The guest half of the host-command channel. It speaks the spool protocol in
# compostbin-core's `host` module, and is a shell script because the container is
# Linux while compostbin itself is a macOS binary.
COPY compostbin-host /usr/local/bin/compostbin-host
RUN chmod +x /usr/local/bin/compostbin-host

RUN useradd -m -s /bin/bash claude
# Mounted from the host at run time (D5); created here so the first run has a
# mountpoint even before the host directory exists.
RUN mkdir -p /home/claude/.claude

# Keeps `.claude.json` — the account and the onboarding answers — inside the
# mount. Without it Claude writes to ~/.claude.json, which dies with the
# container, and every restart asks for a fresh login.
ENV CLAUDE_CONFIG_DIR=/home/claude/.claude

USER claude
WORKDIR /home/claude
