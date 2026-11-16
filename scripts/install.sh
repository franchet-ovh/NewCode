#!/bin/bash

cargo build --release
python3 scripts/addons.py --install

cp target/release/focus-editor ~/.local/share/focus-editor/bin/focus-editor
cp target/release/focus-editor-server ~/.local/share/focus-editor/bin/focus-editor-server