#!/bin/sh
# Runs once, at the first boot's rc.local time (nuageinit_user_data_script).
# Leaves a shell on the single-port virtio console, /dev/ttyV0.0, at every boot.

mkdir -p /usr/local/libexec

cat > /usr/local/libexec/spike-agent <<'EOF'
#!/bin/sh
# One shell only: two reading the port would split the host's commands.
[ -n "$SPIKE_AGENT_LOCKED" ] ||
  SPIKE_AGENT_LOCKED=1 exec lockf -s -t 0 /var/run/spike-agent.lock "$0"

port=/dev/ttyV0.0
exec <"$port" >"$port" 2>&1
# After opening: a tty takes its initial settings on open.
stty raw -echo
boot=$(sysctl -n kern.boottime | sed 's/^{ sec = \([0-9]*\),.*/\1/')
echo "spike-ready $(( $(date +%s) - boot ))"
# Through a pipe, so the shell is not interactive: no prompts.
cat | /bin/sh
EOF
chmod 755 /usr/local/libexec/spike-agent

start='/usr/sbin/daemon -r -f /usr/local/libexec/spike-agent'
grep -qF "$start" /etc/rc.local 2>/dev/null || echo "$start" >> /etc/rc.local

# The image boots at once already (autoboot_delay=-1). Later boots skip the
# first boot's package upgrade, which needs the network.
grep -q '^firstboot_pkg_upgrade_enable="NO"' /etc/rc.conf ||
  echo 'firstboot_pkg_upgrade_enable="NO"' >> /etc/rc.conf

$start
