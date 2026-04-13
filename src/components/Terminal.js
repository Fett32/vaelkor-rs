/**
 * Terminal — multi-tab xterm.js terminal panel.
 *
 * Architecture note (from Vaelkor design doc):
 *   tmux *owns* sessions.  xterm.js is view-only — it renders output piped
 *   from the Rust backend (tauri-plugin-shell) but does not own the PTY.
 *
 * For the initial release this component:
 *   - Renders one xterm.js Terminal per tab.
 *   - Pipes stdin to the Rust shell plugin so the user can interact.
 *   - Listens for the "agent-selected" event from AgentPanel and opens (or
 *     focuses) a tab named after the agent's tmux session.
 *   - Falls back gracefully when xterm.js is not available (dev/build
 *     without the npm package installed).
 *
 * xterm.js is loaded as a dynamic import so the rest of the app boots even
 * if the package is missing during early scaffolding.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------
// Tab model
// ---------------------------------------------------------------------------

/**
 * @typedef {{ id: string, label: string, $slot: HTMLElement, term: any|null }} Tab
 */

/** @type {Tab[]} */
let tabs = [];

/** ID of the currently visible tab. */
let activeTabId = null;

/** Incrementing counter for default tab labels. */
let tabCounter = 0;

// ---------------------------------------------------------------------------
// DOM refs
// ---------------------------------------------------------------------------

let $tabs;        // #terminal-tabs
let $container;   // #terminal-container
let $addBtn;      // #term-add-tab

// xterm.js Terminal constructor (null when package unavailable).
let XTerm = null;
let FitAddon = null;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/** Map from tab label to agent ID (for routing input). */
const tabAgentMap = new Map();

/** Track last content per agent to avoid redundant writes. */
const lastContent = new Map();

/**
 * Initialise the terminal panel.  Must be called after DOM is ready.
 */
export async function initTerminal() {
  $tabs      = document.getElementById("terminal-tabs");
  $container = document.getElementById("terminal-container");
  $addBtn    = document.getElementById("term-add-tab");

  $addBtn.addEventListener("click", () => openTab());

  // Listen for agent selection from the sidebar.
  document.addEventListener("agent-selected", async (e) => {
    const agent = e.detail?.agent;
    if (!agent) return;
    // Use consistent labeling: vaelkor-{agent_id}
    const label = `vaelkor-${agent.id}`;
    // Map this tab to the agent for input routing.
    tabAgentMap.set(label, agent.id);

    // Reuse existing tab with same label, or open a new one.
    const existing = tabs.find((t) => t.label === label);
    if (existing) {
      activateTab(existing.id);
    } else {
      openTab(label, agent.id);
    }

    // Always attach/re-attach to ensure streaming is active and content is fresh.
    try {
      const initialContent = await invoke("terminal_attach", { agentId: agent.id });
      const tab = tabs.find((t) => t.label === label);
      if (tab?.term && initialContent) {
        tab.term.write("\x1b[2J\x1b[H" + initialContent);
      }
    } catch (err) {
      console.warn("[Terminal] attach failed:", err);
    }
  });

  // Listen for terminal output events from the backend.
  listen("terminal-output", (event) => {
    const { agent_id, data } = event.payload;
    if (!agent_id || !data) return;

    const label = `vaelkor-${agent_id}`;

    // Skip if content unchanged (compare by length + first/last chars for speed).
    const contentKey = `${data.length}:${data.slice(0,50)}:${data.slice(-50)}`;
    if (lastContent.get(agent_id) === contentKey) return;
    lastContent.set(agent_id, contentKey);

    // Find the tab for this agent (don't auto-create, user must click first).
    const tab = tabs.find((t) => t.label === label);
    if (!tab?.term) return;

    // Clear screen and write new content.
    tab.term.write("\x1b[2J\x1b[H" + data);
  });

  // Attempt to load xterm.js dynamically.
  await loadXterm();

  // Open a default "system" tab.
  openTab("system");
}

// ---------------------------------------------------------------------------
// xterm.js lazy load
// ---------------------------------------------------------------------------

async function loadXterm() {
  try {
    const mod = await import("@xterm/xterm");
    XTerm = mod.Terminal;

    // Try the fit addon (optional — keeps terminal filling its container).
    try {
      const fitMod = await import("@xterm/addon-fit");
      FitAddon = fitMod.FitAddon;
    } catch {
      // Fit addon missing — xterm will still work, just won't auto-resize.
    }
  } catch {
    console.warn(
      "[Terminal] @xterm/xterm not installed — falling back to plain text output. " +
        "Run `npm install @xterm/xterm @xterm/addon-fit` to enable full terminal support."
    );
  }
}

// ---------------------------------------------------------------------------
// Tab lifecycle
// ---------------------------------------------------------------------------

let _idCounter = 0;
function newId() { return `tab-${++_idCounter}`; }

/**
 * Open a new terminal tab (and optionally activate it immediately).
 * @param {string} [label]  Human-readable tab name.
 * @param {string} [agentId]  Agent ID for input routing.
 * @returns {string}  The new tab's ID.
 */
function openTab(label, agentId) {
  const id = newId();
  tabCounter++;
  const tabLabel = label ?? `terminal ${tabCounter}`;

  // Track agent association if provided.
  if (agentId) {
    tabAgentMap.set(tabLabel, agentId);
  }

  // Create xterm slot div.
  const $slot = document.createElement("div");
  $slot.className = "xterm-slot";
  $slot.id = `slot-${id}`;
  $container.appendChild($slot);

  /** @type {any|null} */
  let term = null;
  let fitAddon = null;

  if (XTerm) {
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

    term.open($slot);
    if (fitAddon) fitAddon.fit();

    term.writeln(`\x1b[35mVaelkor\x1b[0m — tab: \x1b[33m${tabLabel}\x1b[0m`);
    term.writeln("─".repeat(40));
    term.write("\r\n");

    // Wire up user input → tmux via Rust backend.
    term.onData((data) => {
      const agentId = tabAgentMap.get(tabLabel);
      if (agentId) {
        invoke("terminal_send_keys", { agentId: agentId, keys: data }).catch((e) => {
          console.warn("[Terminal] send_keys failed:", e);
        });
      }
    });

    // Refit on container resize.
    const ro = new ResizeObserver(() => {
      if (fitAddon) fitAddon.fit();
    });
    ro.observe($slot);

  } else {
    // Fallback: plain <pre> output area.
    $slot.innerHTML =
      `<div id="terminal-fallback">` +
      `<span style="color:#7c6ff7">Vaelkor</span> — tab: ` +
      `<span style="color:#f0c040">${escHtml(tabLabel)}</span>\n` +
      `<span style="color:#55556a">xterm.js not installed — install @xterm/xterm for full terminal support.</span>` +
      `</div>`;
  }

  /** @type {Tab} */
  const tab = { id, label: tabLabel, $slot, term, fitAddon };
  tabs.push(tab);

  // Render the tab button (insert before the + button).
  const $btn = buildTabButton(tab);
  $tabs.insertBefore($btn, $addBtn);

  activateTab(id);
  return id;
}

function closeTab(id) {
  const idx = tabs.findIndex((t) => t.id === id);
  if (idx < 0) return;

  const tab = tabs[idx];

  // Detach streaming for this agent.
  const agentId = tabAgentMap.get(tab.label);
  if (agentId) {
    invoke("terminal_detach", { agentId: agentId }).catch(() => {});
    tabAgentMap.delete(tab.label);
    lastContent.delete(agentId);
  }

  // Dispose xterm instance.
  if (tab.term) tab.term.dispose();

  // Remove DOM nodes.
  tab.$slot.remove();
  document.getElementById(`tabBtn-${id}`)?.remove();

  tabs.splice(idx, 1);

  // Activate adjacent tab if this one was active.
  if (activeTabId === id) {
    const next = tabs[Math.min(idx, tabs.length - 1)];
    if (next) {
      activateTab(next.id);
    } else {
      activeTabId = null;
    }
  }
}

function activateTab(id) {
  activeTabId = id;

  // Toggle slot visibility.
  for (const tab of tabs) {
    tab.$slot.classList.toggle("active", tab.id === id);
  }

  // Toggle tab button active state.
  for (const tab of tabs) {
    document.getElementById(`tabBtn-${tab.id}`)
      ?.classList.toggle("active", tab.id === id);
  }

  // Refit the newly visible terminal.
  const active = tabs.find((t) => t.id === id);
  if (active?.fitAddon) active.fitAddon.fit();
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/**
 * Build a tab button element for the tab bar.
 * @param {Tab} tab
 * @returns {HTMLElement}
 */
function buildTabButton(tab) {
  const $btn = document.createElement("div");
  $btn.className = "term-tab";
  $btn.id = `tabBtn-${tab.id}`;
  $btn.title = tab.label;

  const $label = document.createElement("span");
  $label.textContent = tab.label;

  const $close = document.createElement("span");
  $close.className = "close-tab";
  $close.textContent = "×";
  $close.title = "Close tab";
  $close.addEventListener("click", (e) => {
    e.stopPropagation();
    closeTab(tab.id);
  });

  $btn.append($label, $close);
  $btn.addEventListener("click", () => activateTab(tab.id));

  return $btn;
}

// ---------------------------------------------------------------------------
// Public write API — other modules can pipe text into the active terminal.
// ---------------------------------------------------------------------------

/**
 * Write text to the currently active terminal tab.
 * @param {string} text  May contain ANSI escape codes.
 */
export function writeToActive(text) {
  const tab = tabs.find((t) => t.id === activeTabId);
  if (!tab) return;

  if (tab.term) {
    tab.term.write(text);
  } else {
    const fallback = tab.$slot.querySelector("#terminal-fallback");
    if (fallback) fallback.textContent += text;
  }
}

/**
 * Write text to a specific tab by label (creates the tab if absent).
 * @param {string} label
 * @param {string} text
 */
export function writeToTab(label, text) {
  let tab = tabs.find((t) => t.label === label);
  if (!tab) openTab(label);
  tab = tabs.find((t) => t.label === label);
  if (!tab) return;

  if (tab.term) {
    tab.term.write(text);
  } else {
    const fallback = tab.$slot.querySelector("#terminal-fallback");
    if (fallback) fallback.textContent += text;
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function escHtml(str) {
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}
