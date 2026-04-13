/**
 * Terminal — single xterm.js instance rendering vaelkor-main.
 *
 * Architecture:
 *   tmux owns all sessions. vaelkor-main is a display session with panes
 *   linked to individual agent sessions. This component renders that one
 *   session. tmux handles tiling, focus, and input routing.
 *
 *   User keystrokes go to vaelkor-main via the Rust backend.
 *   Terminal output streams from the backend's polling loop.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

let $container;
let term = null;
let fitAddon = null;
let XTerm = null;
let FitAddon = null;
let lastContentKey = null;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Initialise the terminal. Must be called after DOM is ready.
 */
export async function initTerminal() {
  $container = document.getElementById("terminal-container");

  await loadXterm();
  createTerminal();

  // Listen for terminal output from the backend (single stream).
  listen("terminal-output", (event) => {
    const { data } = event.payload;
    if (!data || !term) return;

    // Skip if content unchanged.
    const contentKey = `${data.length}:${data.slice(0, 50)}:${data.slice(-50)}`;
    if (lastContentKey === contentKey) return;
    lastContentKey = contentKey;

    // Clear and rewrite (capture-pane sends full screen each time).
    term.write("\x1b[2J\x1b[H" + data);
  });

  // Auto-attach to vaelkor-main.
  attach();
}

// ---------------------------------------------------------------------------
// xterm.js setup
// ---------------------------------------------------------------------------

async function loadXterm() {
  try {
    const mod = await import("@xterm/xterm");
    XTerm = mod.Terminal;
    try {
      const fitMod = await import("@xterm/addon-fit");
      FitAddon = fitMod.FitAddon;
    } catch { /* fit addon optional */ }
  } catch {
    console.warn(
      "[Terminal] @xterm/xterm not installed — run `npm install @xterm/xterm @xterm/addon-fit`"
    );
  }
}

function createTerminal() {
  if (!XTerm) {
    $container.innerHTML =
      `<div id="terminal-fallback">` +
      `<span style="color:#7c6ff7">Vaelkor</span> — xterm.js not installed.\n` +
      `<span style="color:#55556a">Run: npm install @xterm/xterm @xterm/addon-fit</span>` +
      `</div>`;
    return;
  }

  term = new XTerm({
    theme: {
      background:  "#0a0a0c",
      foreground:  "#e0e0f0",
      cursor:      "#7c6ff7",
      selectionBackground: "rgba(124,111,247,0.3)",
      black:       "#15151a",
      red:         "#d06060",
      green:       "#4ec94e",
      yellow:      "#f0c040",
      blue:        "#7c6ff7",
      magenta:     "#b06ab0",
      cyan:        "#5bc8af",
      white:       "#e0e0f0",
      brightBlack: "#55556a",
    },
    fontFamily: "'JetBrains Mono', 'Cascadia Code', 'Fira Code', monospace",
    fontSize: 13,
    lineHeight: 1.4,
    cursorBlink: true,
    allowProposedApi: true,
  });

  if (FitAddon) {
    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
  }

  term.open($container);
  if (fitAddon) fitAddon.fit();

  term.writeln(`\x1b[35mVaelkor\x1b[0m — vaelkor-main`);
  term.writeln("\u2500".repeat(40));
  term.write("\r\n");

  // User input → vaelkor-main via backend.
  term.onData((data) => {
    invoke("terminal_send_keys", { keys: data }).catch((e) => {
      console.warn("[Terminal] send_keys failed:", e);
    });
  });

  // Refit on container resize.
  const ro = new ResizeObserver(() => {
    if (fitAddon) fitAddon.fit();
  });
  ro.observe($container);
}

// ---------------------------------------------------------------------------
// Attach to vaelkor-main
// ---------------------------------------------------------------------------

async function attach() {
  // Retry until vaelkor-main exists (pane manager may still be creating it).
  for (let i = 0; i < 20; i++) {
    try {
      const initialContent = await invoke("terminal_attach");
      if (term && initialContent) {
        term.write("\x1b[2J\x1b[H" + initialContent);
      }
      return;
    } catch (err) {
      console.warn(`[Terminal] attach attempt ${i + 1}: ${err}`);
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  console.error("[Terminal] failed to attach to vaelkor-main after retries");
}

// ---------------------------------------------------------------------------
// Public write API (for other modules to pipe text)
// ---------------------------------------------------------------------------

/**
 * Write text to the terminal.
 * @param {string} text  May contain ANSI escape codes.
 */
export function writeToTerminal(text) {
  if (term) {
    term.write(text);
  }
}
