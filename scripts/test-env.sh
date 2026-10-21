#!/usr/bin/env bash
# One-command local lab for mihomo-top.
#
#   scripts/test-env.sh                 # Linux: install mihomo, run unit + live tests
#   scripts/test-env.sh live-network    # also dial example.com (optional, needs network)
#   scripts/test-env.sh check           # exit 0 on Linux, exit 2 anywhere else
#
# The lab is loopback-only: DIRECT plus a mock HTTP upstream, a fake egress
# address (203.0.113.10), and no subscription. TUN is probed and then left
# down; mihomo itself is started with tun.enable false.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-test}"
MIHOMO_VERSION="v1.19.32"
ASSET_SHA256="8451100836c9eda194331c2babfad490b2faf30cebc1a04b0e76fd8ac2d35d10"
BIN_SHA256="3122d100e8177501776109f1a6253a694611627cf4d7c7ec82705855cf8626a8"
ASSET_NAME="mihomo-linux-amd64-${MIHOMO_VERSION}.gz"
ASSET_URL="https://github.com/MetaCubeX/mihomo/releases/download/${MIHOMO_VERSION}/${ASSET_NAME}"
BIN_DIR="${ROOT}/.cache/mihomo/${MIHOMO_VERSION}"
BIN_PATH="${BIN_DIR}/mihomo"

usage() {
    echo "usage: $0 [test|live-network|check]" >&2
    exit 2
}

platform_check() {
    if [[ "$(uname -s)" != "Linux" ]]; then
        echo "mihomo-top is Linux-only (/proc attribution; src/lib.rs refuses other targets)." >&2
        echo "This host is $(uname -s). The lab was not started." >&2
        exit 2
    fi
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 1
    fi
}

ensure_tmux() {
    if command -v tmux >/dev/null 2>&1; then
        return
    fi
    if command -v sudo >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
        sudo apt-get update
        sudo apt-get install -y tmux
        return
    fi
    echo "tmux is required for pane attribution in the live lab" >&2
    exit 1
}

install_mihomo() {
    if [[ -x "${BIN_PATH}" ]] && echo "${BIN_SHA256}  ${BIN_PATH}" | sha256sum -c - >/dev/null; then
        return
    fi
    need_cmd curl
    need_cmd gzip
    need_cmd sha256sum
    mkdir -p "${BIN_DIR}"
    local archive="${BIN_DIR}/${ASSET_NAME}"
    curl -fL --retry 3 --retry-delay 2 -o "${archive}" "${ASSET_URL}"
    echo "${ASSET_SHA256}  ${archive}" | sha256sum -c -
    gzip -dc "${archive}" > "${BIN_PATH}.partial"
    chmod 755 "${BIN_PATH}.partial"
    echo "${BIN_SHA256}  ${BIN_PATH}.partial" | sha256sum -c -
    mv "${BIN_PATH}.partial" "${BIN_PATH}"
    rm -f "${archive}"
}

run_tests() {
    need_cmd cargo
    need_cmd python3
    platform_check
    ensure_tmux
    install_mihomo
    export MIHOMO_BIN="${BIN_PATH}"
    cd "${ROOT}"
    if [[ "${MODE}" == "live-network" ]]; then
        export MIHOMO_TOP_LIVE_NETWORK=1
    fi
    cargo test --locked --test scenario_report --test controller_fixture
    cargo test --locked --test live_controller -- --ignored --test-threads=1 --nocapture
}

case "${MODE}" in
    check)
        platform_check
        echo "linux ok"
        ;;
    test|demo|live-network)
        run_tests
        ;;
    *)
        usage
        ;;
esac
