#!/usr/bin/env bash
# Install (or remove) the single `mihomo-top` launcher.
#
#   mihomo-top        TUI (auto-detected); args pass through: --once, --stream, ...
#
# The wrapper goes to ~/.local/bin (on PATH by default on Ubuntu via
# ~/.profile) — no shell-startup edits. Idempotent; --remove uninstalls.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
TARGET="${REPO_ROOT}/target/release/mihomo-top"
NAME="mihomo-top"

case "${1:-install}" in
    install)
        mkdir -p "${BIN_DIR}"
        cat > "${BIN_DIR}/${NAME}" <<WRAPPER
#!/usr/bin/env bash
# Installed by ${REPO_ROOT}/scripts/install.sh — safe to delete.
target="${TARGET}"
if [[ ! -x "\$target" ]]; then
    echo "${NAME}: target missing: \$target" >&2
    echo "  hint: cd ${REPO_ROOT} && cargo build --release" >&2
    exit 1
fi
exec "\$target" "\$@"
WRAPPER
        chmod +x "${BIN_DIR}/${NAME}"
        echo "installed ${BIN_DIR}/${NAME} -> ${TARGET}"
        ;;
    remove)
        rm -f "${BIN_DIR}/${NAME}"
        echo "removed ${BIN_DIR}/${NAME}"
        ;;
    *)
        echo "usage: $0 [install|remove]" >&2
        exit 2
        ;;
esac
