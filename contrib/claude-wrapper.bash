# shellcheck shell=bash
# Route Claude Code through Smart-Claude-Router when the proxy is running, and connect to
# Anthropic directly when it isn't. Add this line to ~/.bashrc:
#
#   source /path/to/Smart-Claude-Router/contrib/claude-wrapper.bash
#
# Set SMART_CLAUDE_ROUTER_ADDR if the proxy listens somewhere other than 127.0.0.1:8899.
# Bypass the proxy once: `command claude`. Stop using it: remove the `source` line.

claude() {
    local addr="${SMART_CLAUDE_ROUTER_ADDR:-127.0.0.1:8899}"
    # Only checks that something accepts connections there; sends no request.
    if (exec 3<>"/dev/tcp/${addr%:*}/${addr##*:}") 2>/dev/null; then
        ANTHROPIC_BASE_URL="http://${addr}" command claude "$@"
    else
        echo "smart-claude-router is not running on ${addr}; connecting to Anthropic directly" >&2
        command claude "$@"
    fi
}
