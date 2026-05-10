#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT/worker"

if [ ! -d .venv ]; then
    python3 -m venv .venv
fi
.venv/bin/pip install -U pip
.venv/bin/pip install -e .

echo "worker venv ready at $REPO_ROOT/worker/.venv"
