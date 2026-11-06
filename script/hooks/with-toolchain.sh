#!/bin/sh
# Run a command with the pinned toolchains. Git hooks often run in shells (GUI clients, agent
# sessions) where mise isn't activated and rustup's bin folder isn't on PATH, so PATH can't be
# trusted.
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ -d "$cargo_bin" ]; then
  PATH="$cargo_bin:$PATH"
  export PATH
fi
if command -v mise >/dev/null 2>&1; then
  exec mise exec -- "$@"
fi
exec "$@"
