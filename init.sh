#!/usr/bin/env bash
# Byte-compile feynman-chiron-package Emacs Lisp files.
# feynman-chiron.el depends only on built-in packages (json, url).
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
MY_DIR="$(realpath "$DIR/../my")"

compile() {
    local file="$1"
    local out exit_code=0
    out=$(emacs --batch \
          --eval "(setq load-path (append '(\"$MY_DIR\" \"$DIR\") load-path))" \
          --eval "(setq byte-compile-error-on-warn nil)" \
          --eval "(setq byte-compile-warnings nil)" \
          -f batch-byte-compile "$file" 2>&1) || exit_code=$?
    if [ "$exit_code" -ne 0 ]; then
        echo "ERROR: failed to compile $(basename "$file")" >&2
        echo "$out" >&2
        exit "$exit_code"
    fi
}

compile "$DIR/feynman-chiron.el"