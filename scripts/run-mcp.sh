#!/bin/sh
set -eu

plugin_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)

if ! command -v cargo >/dev/null 2>&1; then
    echo "whatsapp-mcp requires Rust and cargo in PATH" >&2
    exit 127
fi

if [ -n "${WHATSAPP_MCP_BUILD_CACHE:-}" ]; then
    build_cache=$WHATSAPP_MCP_BUILD_CACHE
elif [ -n "${XDG_CACHE_HOME:-}" ]; then
    build_cache=$XDG_CACHE_HOME/whatsapp-mcp
else
    : "${HOME:?whatsapp-mcp requires HOME or WHATSAPP_MCP_BUILD_CACHE}"
    build_cache=$HOME/.cache/whatsapp-mcp
fi

target_dir=$build_cache/target

cargo build \
    --quiet \
    --locked \
    --release \
    --manifest-path "$plugin_root/Cargo.toml" \
    --package wa-mcp-server \
    --bin wa-mcp-server \
    --target-dir "$target_dir"

exec "$target_dir/release/wa-mcp-server"
