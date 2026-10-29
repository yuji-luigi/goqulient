#!/bin/bash
# Development helper. Everything Rust and Node runs inside the compose project's containers (docker-compose.yml), so
# the only thing you need on your machine is Docker.
#
#   scripts/dev.sh up                    start the test databases + the `engine` and `vscode` containers
#   scripts/dev.sh engine test [filter]  cargo test (unit, integration and parity tests) in the `engine` container
#   scripts/dev.sh engine check          rustfmt --check + clippy -D warnings
#   scripts/dev.sh engine build          release build (Linux)
#   scripts/dev.sh engine shell          shell in the `engine` container
#   scripts/dev.sh vscode test           engine debug build, then type check + Node tests against it
#   scripts/dev.sh vscode build          dist/ (extension + webviews)
#   scripts/dev.sh vscode e2e            the extension inside a real VS Code (xvfb) against the test databases
#   scripts/dev.sh vscode package <dir>  one VSIX per engine binary under <dir> (in the repo) into dist/
#   scripts/dev.sh vscode run [folder]   native engine (needs cargo) + this extension in a new VS Code window
#   scripts/dev.sh vscode shell          shell in the `vscode` container
#   scripts/dev.sh down                  stop and delete everything (containers + volumes)
#   scripts/dev.sh env                   the project name and ports, as exports (eval it before a bare `docker compose`)
set -euo pipefail
cd "$(dirname "$0")/.."
cmd="${1:-help}"; shift || true

# One compose project per checkout, so parallel git worktrees never share containers, mounts or test databases
# (docs/TESTING.md#parallel-worktrees). The main checkout keeps `goqulient` and the default ports; a linked worktree is
# named after its folder, gets host ports derived from that name and a smaller engine container, so two fit in Docker.
if [[ -z "${COMPOSE_PROJECT_NAME:-}" ]]; then
  COMPOSE_PROJECT_NAME=goqulient
  if [[ "$(git rev-parse --git-dir 2>/dev/null)" != "$(git rev-parse --git-common-dir 2>/dev/null)" ]]; then
    COMPOSE_PROJECT_NAME="$(basename "$PWD" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9_\n-' '-')"
  fi
fi
export COMPOSE_PROJECT_NAME
if [[ "$COMPOSE_PROJECT_NAME" != goqulient ]]; then
  off=$(( $(printf %s "$COMPOSE_PROJECT_NAME" | cksum | cut -d' ' -f1) % 999 + 1 ))
  export GOKU_PG_PORT="${GOKU_PG_PORT:-$((25432 + off))}" GOKU_MYSQL_PORT="${GOKU_MYSQL_PORT:-$((23306 + off))}" \
    GOKU_MONGO_PORT="${GOKU_MONGO_PORT:-$((27217 + off))}" GOKU_ENGINE_MEM="${GOKU_ENGINE_MEM:-3g}" \
    GOKU_ENGINE_CPUS="${GOKU_ENGINE_CPUS:-2}"
fi

in_vscode() { docker compose exec -T vscode bash -c "$*"; }
in_engine() { docker compose exec -T engine bash -c "$*"; }   # not -l: /etc/profile drops cargo from PATH

case "$cmd" in
  up)
    docker compose up -d --wait ;;
  engine)   # integration tests share the test databases: don't run two test runs at once
    docker compose up -d engine >/dev/null
    case "${1:-test}" in
      test)  in_engine "timeout -s KILL 900 cargo test --workspace ${2:+'$2'}" ;;
      build) in_engine "cargo build --workspace --release" ;;
      check) in_engine "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings" ;;
      shell) docker compose exec engine bash ;;
      *) echo "usage: scripts/dev.sh engine [test [filter] | build | check | shell]" >&2; exit 1 ;;
    esac ;;
  vscode)
    if [[ "${1:-}" == "run" ]]; then   # native engine + this checkout's extension in your VS Code (a new window)
      if ! command -v cargo >/dev/null; then
        cat >&2 <<'EOS'
The extension needs a native goku-engine-server, and cargo isn't installed. Either:
  1. install Rust (https://rustup.rs), then run this again;
  2. or download the VSIX for your platform from the "Extension release" workflow's artifacts on GitHub
     (Actions → Extension release → Run workflow, or a finished run) and install it:
     code --install-extension goqulient-<platform>.vsix
EOS
        exit 1
      fi
      (cd engine && cargo build -p goku-engine-server)
      if command -v npm >/dev/null; then
        (cd vscode && npm ci --no-audit --no-fund && npm run build)
      else   # dist/ is plain JavaScript, so the Docker build works on any host
        docker compose up -d vscode >/dev/null && in_vscode "npm ci --no-audit --no-fund && npm run build"
      fi
      command -v code >/dev/null || { echo "Put 'code' on the PATH (VS Code: Shell Command: Install 'code' command in PATH)." >&2; exit 1; }
      exe=goku-engine-server; [[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]] && exe=goku-engine-server.exe
      GOKU_ENGINE_PATH="$PWD/engine/target/debug/$exe" code --new-window \
        --extensionDevelopmentPath="$PWD/vscode" "${2:-$PWD}"
      exit 0
    fi
    docker compose up -d engine vscode >/dev/null
    case "${1:-test}" in
      test)  in_engine "cargo build -p goku-engine-server" && in_vscode "npm ci --no-audit --no-fund && npm run check" ;;
      build) in_vscode "npm ci --no-audit --no-fund && npm run typecheck && npm run build" ;;
      e2e)   # reads the test databases only; GOKU_E2E_SCREENSHOTS=<dir under the repo> keeps window screenshots
        in_engine "cargo build -p goku-engine-server" && in_vscode "npm ci --no-audit --no-fund && npm run build" &&
          docker compose --profile e2e run --rm ${GOKU_E2E_SCREENSHOTS:+-e GOKU_E2E_SCREENSHOTS="/work/$GOKU_E2E_SCREENSHOTS"} \
            vscode-e2e bash -c "timeout -s KILL 600 xvfb-run -a -s '-screen 0 1440x900x24' node test-e2e/run.mjs" ;;
      package)   # engines downloaded from a workflow run (docs/PUBLISHING.md); extra arguments go to vsce, e.g. --pre-release.
        # Packaged from a copy on the container's own disk: see scripts/package-vsix.sh for why not in /work.
        [[ -d "${2:-}" ]] || { echo "usage: scripts/dev.sh vscode package <engines dir in the repo> [vsce options]" >&2; exit 1; }
        in_vscode "npm ci --no-audit --no-fund && rm -rf /tmp/pkg && mkdir -p /tmp/pkg/vscode && cp /work/LICENSE /tmp/pkg/ &&
          tar -C /work/vscode --exclude=./node_modules --exclude=./bin --exclude=./dist -cf - . | tar -C /tmp/pkg/vscode -xf - &&
          cp -a node_modules /tmp/pkg/vscode/ && cd /tmp/pkg/vscode && npm run typecheck && NODE_ENV=production npm run build &&
          /work/scripts/package-vsix.sh '/work/$2' /work/dist ${*:3}" ;;
      shell) docker compose exec vscode bash ;;
      *) echo "usage: scripts/dev.sh vscode [test | build | e2e | run [folder] | package <dir> | shell]" >&2; exit 1 ;;
    esac ;;
  down)
    docker compose --profile e2e down -v ;;
  env)
    for v in COMPOSE_PROJECT_NAME GOKU_PG_PORT GOKU_MYSQL_PORT GOKU_MONGO_PORT GOKU_ENGINE_MEM GOKU_ENGINE_CPUS; do
      [[ -n "${!v:-}" ]] && echo "export $v=${!v}"
    done; true ;;
  *)
    sed -n '2,17p' "$0" ;;
esac
