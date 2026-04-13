/**
 * Terminal — single xterm.js instance with PTY relay to vaelkor-main.
 *
 * Architecture:
 *   The Rust backend spawns `tmux attach -t vaelkor-main` inside a real PTY.
 *   PTY output streams to xterm.js as incremental data (no polling, no
 *   clear+rewrite). User input goes back through the PTY to tmux.
 *   tmux handles tiling and input routing to the active pane.
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

  // Listen for terminal output from PTY relay (incremental — just append).
  listen("terminal-output", (event) => {
    const { data } = event.payload;
    if (!data || !term) return;
    term.write(data);
  });
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
    scrollback: 10000,
  });

  if (FitAddon) {
    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
  }

  term.open($container);
  if (fitAddon) fitAddon.fit();

  // -----------------------------------------------------------------------
  // Clipboard: Ctrl+Shift+C to copy, Ctrl+Shift+V to paste.
  // Tauri webview doesn't wire up terminal clipboard by default, so we
  // intercept the key combos before xterm.js processes them.
  // -----------------------------------------------------------------------
  term.attachCustomKeyEventHandler((event) => {
    if (event.type !== "keydown") return true;

    // Ctrl+Shift+C → copy selection to clipboard
    if (event.ctrlKey && event.shiftKey && event.code === "KeyC") {
      const selection = term.select?.getSelection?.() ?? term.getSelection?.();
      if (selection) {
        navigator.clipboard.writeText(selection).catch(() => {});
      }
      return false;
    }

    // Ctrl+Shift+V → paste from clipboard into PTY
    if (event.ctrlKey && event.shiftKey && event.code === "KeyV") {
      navigator.clipboard.readText().then((text) => {
        if (text) {
          invoke("terminal_send_keys", { keys: text }).catch(() => {});
        }
      }).catch(() => {});
      return false;
    }

    return true;
  });

  // -----------------------------------------------------------------------
  // Key send queue — serialise IPC calls so rapid keypresses are never
  // reordered by the async Tauri bridge. Each send waits for the previous
  // one to complete before writing to the PTY.
  // -----------------------------------------------------------------------
  let sendQueue = Promise.resolve();

  term.onData((data) => {
    sendQueue = sendQueue.then(() =>
      invoke("terminal_send_keys", { keys: data })
    ).catch((e) => {
      console.warn("[Terminal] send_keys failed:", e);
    });
  });

  // Report terminal size to backend so PTY matches xterm.js dimensions.
  function reportSize() {
    if (!term) return;
    invoke("terminal_resize", { cols: term.cols, rows: term.rows }).catch(() => {});
  }

  // Resize on container resize.
  const ro = new ResizeObserver(() => {
    if (fitAddon) fitAddon.fit();
    reportSize();
  });
  ro.observe($container);

  // Also report on xterm resize event.
  term.onResize(() => reportSize());

  // Initial size report.
  reportSize();
}

// ---------------------------------------------------------------------------
// Public write API (for other modules)
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
