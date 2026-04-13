/**
 * TaskList — renders and manages the task list in the main panel.
 *
 * Communicates with the Rust backend via Tauri IPC:
 *   invoke("get_tasks")                                 → Task[]
 *   invoke("assign_task", { title, description, agentId }) → Task
 *   invoke("cancel_task", { id })                       → Task
 *
 * TaskState values (SCREAMING_SNAKE_CASE, as serialised by the Rust backend):
 *   ASSIGNED | ACCEPTED | COMPLETED | BLOCKED | CANCELLED |
 *   REJECTED | TIMED_OUT | INTERRUPTED | RECOVERING | STALE
 */

import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/** @type {Task[]} */
let tasks = [];

/** Active filter string (state value or ""). */
let filterState = "";

/** Active search string (lower-cased). */
let filterSearch = "";

/** Polling interval handle. */
let pollHandle = null;

const POLL_MS = 2000;

// ---------------------------------------------------------------------------
// DOM refs
// ---------------------------------------------------------------------------

let $scroll;
let $search;
let $filterSelect;
let $overlay;
let $titleInput;
let $descInput;
let $agentSelect;
let $btnNew;
let $btnClose;
let $btnSubmit;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Initialise the task list.  Must be called after the DOM is ready.
 */
export function initTaskList() {
  $scroll       = document.getElementById("task-list-scroll");
  $search       = document.getElementById("task-search");
  $filterSelect = document.getElementById("task-filter-state");
  $overlay      = document.getElementById("modal-overlay");
  $titleInput   = document.getElementById("modal-title-input");
  $descInput    = document.getElementById("modal-desc-input");
  $agentSelect  = document.getElementById("modal-agent-select");
  $btnNew       = document.getElementById("btn-new-task");
  $btnClose     = document.getElementById("btn-close-modal");
  $btnSubmit    = document.getElementById("btn-submit-task");

  $btnNew.addEventListener("click", openModal);
  $btnClose.addEventListener("click", closeModal);
  $btnSubmit.addEventListener("click", handleSubmit);

  // Close modal on backdrop click.
  $overlay.addEventListener("click", (e) => {
    if (e.target === $overlay) closeModal();
  });

  // Keyboard: Escape closes modal.
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && $overlay.classList.contains("open")) closeModal();
  });

  $search.addEventListener("input", () => {
    filterSearch = $search.value.toLowerCase();
    renderTasks();
  });

  $filterSelect.addEventListener("change", () => {
    filterState = $filterSelect.value;
    renderTasks();
  });

  fetchTasks();
  pollHandle = setInterval(fetchTasks, POLL_MS);
}

/**
 * Stop polling.  Call when tearing down the UI.
 */
export function destroyTaskList() {
  clearInterval(pollHandle);
}

// ---------------------------------------------------------------------------
// IPC
// ---------------------------------------------------------------------------

async function fetchTasks() {
  try {
    tasks = await invoke("get_tasks");
    renderTasks();
  } catch (err) {
    console.error("[TaskList] get_tasks failed:", err);
  }
}

async function handleSubmit() {
  const title       = $titleInput.value.trim();
  const description = $descInput.value.trim();
  const agentId     = $agentSelect.value || null;

  if (!title) {
    $titleInput.focus();
    return;
  }

  try {
    const task = await invoke("assign_task", {
      title,
      description,
      agentId,
    });

    // Optimistic: push before next poll.
    tasks.push(task);
    renderTasks();
    closeModal();
  } catch (err) {
    console.error("[TaskList] assign_task failed:", err);
    alert(`Failed to create task: ${err}`);
  }
}

async function cancelTask(id) {
  try {
    const updated = await invoke("cancel_task", { id });
    const idx = tasks.findIndex((t) => t.id === id);
    if (idx >= 0) tasks[idx] = updated;
    renderTasks();
  } catch (err) {
    console.error("[TaskList] cancel_task failed:", err);
    alert(`Failed to cancel task: ${err}`);
  }
}

// ---------------------------------------------------------------------------
// Modal helpers
// ---------------------------------------------------------------------------

function openModal() {
  $titleInput.value = "";
  $descInput.value  = "";
  $overlay.classList.add("open");
  $titleInput.focus();
}

function closeModal() {
  $overlay.classList.remove("open");
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function renderTasks() {
  let visible = tasks;

  if (filterState) {
    visible = visible.filter((t) => t.state === filterState);
  }

  if (filterSearch) {
    visible = visible.filter(
      (t) =>
        t.title.toLowerCase().includes(filterSearch) ||
        t.description.toLowerCase().includes(filterSearch) ||
        t.id.toLowerCase().includes(filterSearch)
    );
  }

  // Sort: non-terminal first, then by updated_at descending.
  const terminalStates = new Set([
    "COMPLETED", "CANCELLED", "REJECTED", "TIMED_OUT",
  ]);

  visible = [...visible].sort((a, b) => {
    const aTerm = terminalStates.has(a.state);
    const bTerm = terminalStates.has(b.state);
    if (aTerm !== bTerm) return aTerm ? 1 : -1;
    return new Date(b.updated_at) - new Date(a.updated_at);
  });

  $scroll.innerHTML = "";

  if (visible.length === 0) {
    const empty = document.createElement("p");
    empty.className = "task-empty";
    empty.textContent =
      tasks.length === 0
        ? "No tasks yet. Click + New task to get started."
        : "No tasks match the current filter.";
    $scroll.appendChild(empty);
    return;
  }

  for (const task of visible) {
    $scroll.appendChild(buildTaskCard(task));
  }
}

/**
 * Build a single task card element.
 * @param {Task} task
 * @returns {HTMLElement}
 */
function buildTaskCard(task) {
  const card = document.createElement("div");
  card.className = "task-card";
  card.dataset.taskId = task.id;

  // Title row
  const titleEl = document.createElement("div");
  titleEl.className = "task-title";
  titleEl.textContent = task.title;

  // Description row
  const descEl = document.createElement("div");
  descEl.className = "task-desc";
  descEl.textContent = task.description || "—";

  // Meta row: state badge + assigned agent + timestamp
  const metaEl = document.createElement("div");
  metaEl.className = "task-meta";

  const badge = document.createElement("span");
  badge.className = "state-badge";
  badge.dataset.state = task.state;
  badge.textContent = task.state;

  const agentSpan = document.createElement("span");
  agentSpan.textContent = task.assigned_to
    ? ` · ${task.assigned_to}`
    : " · unassigned";

  const timeSpan = document.createElement("span");
  timeSpan.textContent = " · " + formatRelative(task.updated_at);

  metaEl.append(badge, agentSpan, timeSpan);

  // Actions
  const actions = document.createElement("div");
  actions.className = "task-actions";

  const terminalStates = new Set([
    "COMPLETED", "CANCELLED", "REJECTED", "TIMED_OUT",
  ]);

  if (!terminalStates.has(task.state)) {
    const btnCancel = document.createElement("button");
    btnCancel.className = "cancel";
    btnCancel.textContent = "Cancel";
    btnCancel.addEventListener("click", () => cancelTask(task.id));
    actions.appendChild(btnCancel);
  }

  card.append(titleEl, descEl, metaEl, actions);
  return card;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Format an ISO-8601 timestamp as a human-readable relative string.
 * @param {string} iso
 * @returns {string}
 */
function formatRelative(iso) {
  const diff = Date.now() - new Date(iso).getTime();
  if (diff < 60_000)  return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return new Date(iso).toLocaleDateString();
}
