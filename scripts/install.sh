#!/bin/bash

set -euo pipefail

cargo build --release --bins

cp target/release/focus-editor ~/.local/share/focus-editor/bin/focus-editor
cp target/release/focus-editor-server ~/.local/share/focus-editor/bin/focus-editor-server
