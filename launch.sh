#!/bin/bash
# Launch Vaelkor — kills stale wrappers, starts the Tauri app.
# The app builds and runs via cargo tauri dev.

# Load full environment (cargo, rustup, etc.)
source "$HOME/.bashrc" 2>/dev/null
source "$HOME/.cargo/env" 2>/dev/null

# Kill any orphaned wrapper processes from previous runs.
pkill -f vaelkor-wrapper 2>/dev/null

cd /home/fett/Projects/vaelkor-rs
exec cargo tauri dev
