// Shared by the intake page (app.js) and the escalation inbox (inbox.js).
// Plain JavaScript, no build step, same-origin API. All user-supplied text
// is rendered with textContent, never as HTML.
"use strict";

const STATUS_LABELS = {
  received: "Received", triaging: "Triaging", auto_resolved: "Auto-resolved",
  forwarded: "Forwarded", escalated: "Escalated", resolved: "Resolved", failed: "Failed",
};
const FIX_LABELS = {
  proposed: "Fix proposed", no_changes: "No fix", no_repository: "No repository", failed: "Fix failed",
};

// ---------------------------------------------------------------- helpers

function h(tag, props, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key === "class") el.className = value;
    else if (key === "text") el.textContent = value;
    else if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else el.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

function storage(action, value) {
  try {
    if (action === "get") return window.localStorage.getItem("ticket-handler-token");
    if (action === "set") window.localStorage.setItem("ticket-handler-token", value);
  } catch (_) { /* storage may be unavailable; the token then lasts this page only */ }
  return null;
}
let token = storage("get");

async function api(method, path, body) {
  const headers = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(path, {
    method, headers, body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (response.status === 401) {
    const entered = window.prompt("This server requires an API token (server.api_token):");
    if (entered) {
      token = entered.trim();
      storage("set", token);
      return api(method, path, body);
    }
  }
  const text = await response.text();
  const data = text ? JSON.parse(text) : null;
  if (!response.ok) {
    const error = new Error((data && data.message) || `${response.status} ${response.statusText}`);
    error.status = response.status;
    throw error;
  }
  return data;
}

let toastTimer = null;
function toast(message, kind = "info") {
  const el = document.getElementById("toast");
  el.textContent = message;
  el.className = `toast ${kind}`;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, 5000);
}

const pct = (score) => `${Math.round(score * 100)}%`;
const shortId = (id) => id.slice(0, 8);
const when = (iso) => new Date(iso).toLocaleString();
const badge = (cls, text) => h("span", { class: `badge ${cls}`, text });
const statusBadge = (status) => badge(`status-${status}`, STATUS_LABELS[status] || status);

// A similarity bar. The width is set through the CSSOM: the page's
// Content-Security-Policy forbids inline style attributes, but not this.
function meter(score) {
  const fill = h("span", { class: "meter-fill" });
  fill.style.width = pct(Math.max(0, Math.min(1, score)));
  return h("span", { class: "meter", title: pct(score) }, fill);
}

// ---------------------------------------------------------------- header

const shared = { health: null, escalated: 0, onModeChange: null };

/** Loads /healthz and renders the header. Returns the health, or null. */
async function loadServerInfo() {
  try {
    shared.health = await api("GET", "/healthz");
  } catch (error) {
    toast(`Cannot reach the server: ${error.message}`, "error");
    return null;
  }
  renderHeader();
  return shared.health;
}

async function loadEscalationCount() {
  const response = await api("GET", "/v1/tickets?status=escalated&limit=500");
  shared.escalated = response.tickets.length;
  renderNav();
}

function renderHeader() {
  const hl = shared.health;
  document.getElementById("server-info").replaceChildren(
    h("span", { class: "chip" }, `Knowledge base: ${hl.knowledge_entries}`),
    h("span", { class: "chip" }, hl.fixes_enabled ? "Code fixes: on" : "Code fixes: off"),
    h("span", { class: "chip" },
      hl.repositories.length ? `Repos: ${hl.repositories.join(", ")}` : "No repositories"),
  );
  const toggle = document.getElementById("mode-toggle");
  if (!hl.mode_switchable) {
    toggle.replaceChildren(h("span", { class: "chip accent" },
      hl.auto_triage ? "Automatic mode" : "Step-by-step mode"));
  } else {
    const option = (auto, label) => h("button", {
      type: "button",
      class: hl.auto_triage === auto ? "active" : "",
      "aria-pressed": String(hl.auto_triage === auto),
      onclick: () => setMode(auto),
    }, label);
    toggle.replaceChildren(h("div", { class: "segmented", role: "group", "aria-label": "Triage mode" },
      option(false, "Step-by-step"), option(true, "Automatic")));
  }
  renderNav();
}

function renderNav() {
  const page = document.body.dataset.page;
  const link = (href, id, label, count) => h("a", {
    href, class: `nav-link ${page === id ? "active" : ""}`, "aria-current": page === id ? "page" : null,
  }, label, count ? h("span", { class: "count" }, String(count)) : null);
  document.getElementById("nav").replaceChildren(
    link("/", "intake", "Ticket intake"),
    link("/inbox", "inbox", "Escalation inbox", shared.escalated));
}

async function setMode(auto) {
  if (!shared.health || shared.health.auto_triage === auto) return;
  try {
    await api("PUT", "/v1/settings", { auto_triage: auto });
    toast(auto
      ? "Automatic mode: tickets are triaged, fixed and escalated as they arrive."
      : "Step-by-step mode: new tickets wait for you to run each step.", "success");
  } catch (error) {
    toast(error.message, "error");
  }
  await loadServerInfo();
  if (shared.onModeChange) shared.onModeChange(shared.health.auto_triage);
}

/** "12s ago", "3m ago", or a date for anything older than a day. */
function ago(iso) {
  const seconds = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return new Date(iso).toLocaleDateString();
}

/** The preliminary resolution drafted for an escalated ticket. */
function renderDraft(draft) {
  return h("div", { class: "draft" },
    h("div", { class: "draft-head" },
      h("h4", {}, "Preliminary resolution"),
      badge("draft-badge", "Draft for expert review")),
    h("h5", {}, "Suggested first steps"),
    h("ol", { class: "draft-steps" }, draft.first_steps.map((step) => h("li", { text: step }))),
    h("h5", {}, "Similar past cases"),
    draft.precedents.length
      ? h("ul", { class: "precedents" }, draft.precedents.map((p) => h("li", {},
        h("div", {}, h("span", { class: "precedent-title", text: p.title }), " ",
          h("span", { class: "muted mono", text: `${pct(p.score)} similar` })),
        h("div", { class: "muted", text: p.resolution }))))
      : h("p", { class: "muted" }, "None close enough to rely on. This looks new."));
}
