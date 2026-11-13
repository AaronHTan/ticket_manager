// Ticket Handler demo UI. Plain JavaScript, no build step, same-origin API.
// All user-supplied text is rendered with textContent, never as HTML.
"use strict";

const EXAMPLES = [
  {
    label: "Checkout crash on empty cart",
    hint: "A bug seen before, and its fix was stored as a patch. Triage forwards it, then the fix step re-applies the patch to the web-app repository.",
    ticket: {
      title: "Checkout returns 500 on empty cart",
      description: "Submitting checkout with an empty cart crashes with a 500 error and a stack trace.",
      kind: "bug", components: "checkout", repository: "web-app", reporter: "dana@acme.test",
    },
  },
  {
    label: "Password reset email missing",
    hint: "Near-identical to a resolved ticket whose fix is marked automatable, so triage resolves it without a human.",
    ticket: {
      title: "Password reset email not received",
      description: "User requested a password reset but the reset email never arrived. Spam folder checked.",
      kind: "question", components: "auth", reporter: "sam@acme.test",
    },
  },
  {
    label: "Duplicate subscription charge",
    hint: "A known billing issue, but its fix is not automatable, so it goes to L2 with a suggested fix. The fix step runs, finds no stored patch close enough, and changes nothing.",
    ticket: {
      title: "Duplicate charge on subscription invoice",
      description: "Customer was charged twice for the monthly subscription invoice again this month.",
      kind: "bug", components: "billing", reporter: "priya@acme.test",
    },
  },
  {
    label: "Suspicious admin logins",
    hint: "Security keywords make it critical, so it always goes to the security expert and is never fixed automatically.",
    ticket: {
      title: "Possible unauthorized access to admin account",
      description: "Admin saw logins from an unknown country overnight and suspects a security breach.",
      kind: "incident", components: "auth", severity: "high", reporter: "alex@acme.test",
    },
  },
  {
    label: "Dark mode request",
    hint: "Nothing similar has been seen before, so it is escalated as novel. Then try the learning loop: resolve it with \u201csafe to apply automatically\u201d ticked, and create it again. This time triage auto-resolves it.",
    ticket: {
      title: "Dark mode for the reporting page",
      description: "It would be great to have a dark theme option when viewing charts in the evening.",
      kind: "feature_request", components: "frontend", reporter: "Jordan",
    },
  },
];

const STATUS_LABELS = {
  received: "Received", triaging: "Triaging", auto_resolved: "Auto-resolved",
  forwarded: "Forwarded", escalated: "Escalated", resolved: "Resolved", failed: "Failed",
};
const FIX_LABELS = {
  proposed: "Fix proposed", no_changes: "No fix", no_repository: "No repository", failed: "Fix failed",
};

const state = {
  health: null,
  tickets: [],
  selectedId: null,
  view: null,
  viewKey: "",
  notes: {}, // per-ticket messages, such as why a fix was refused
  busy: false,
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

// ---------------------------------------------------------------- server info

async function loadHealth() {
  try {
    state.health = await api("GET", "/healthz");
  } catch (error) {
    toast(`Cannot reach the server: ${error.message}`, "error");
    return;
  }
  const hl = state.health;
  const chips = document.getElementById("server-info");
  chips.replaceChildren(
    h("span", { class: `chip ${hl.auto_triage ? "" : "accent"}` },
      hl.auto_triage ? "Automatic mode" : "Step-by-step mode"),
    h("span", { class: "chip" }, `Knowledge base: ${hl.knowledge_entries} resolved tickets`),
    h("span", { class: "chip" }, hl.fixes_enabled ? "Code fixes: on" : "Code fixes: off"),
    h("span", { class: "chip" },
      hl.repositories.length ? `Repositories: ${hl.repositories.join(", ")}` : "No repositories"),
  );
  const repoSelect = document.querySelector("#new-ticket select[name=repository]");
  const chosen = repoSelect.value;
  repoSelect.replaceChildren(h("option", { value: "" }, "Not given"),
    ...hl.repositories.map((r) => h("option", { value: r }, r)));
  if (hl.repositories.includes(chosen)) repoSelect.value = chosen;
}

// ---------------------------------------------------------------- create form

function setupForm() {
  const select = document.getElementById("example");
  select.replaceChildren(
    h("option", { value: "" }, "Blank ticket"),
    ...EXAMPLES.map((e, i) => h("option", { value: String(i) }, e.label)),
  );
  select.addEventListener("change", () => applyExample(select.value));
  document.getElementById("new-ticket").addEventListener("submit", submitTicket);
  document.getElementById("refresh").addEventListener("click", () => refresh(true));
  select.value = "0";
  applyExample("0");
}

function applyExample(index) {
  const form = document.getElementById("new-ticket");
  const example = EXAMPLES[Number(index)];
  const t = example ? example.ticket : {};
  form.title.value = t.title || "";
  form.description.value = t.description || "";
  form.kind.value = t.kind || "bug";
  form.severity.value = t.severity || "";
  form.components.value = t.components || "";
  form.reporter.value = t.reporter || "";
  const repoOption = [...form.repository.options].some((o) => o.value === t.repository);
  form.repository.value = repoOption ? t.repository : "";
  document.getElementById("example-hint").textContent = example ? example.hint : "";
}

async function submitTicket(event) {
  event.preventDefault();
  const form = event.target;
  const body = {
    client: form.client.value.trim(),
    title: form.title.value.trim(),
    description: form.description.value.trim(),
    kind: form.kind.value,
    components: form.components.value.split(",").map((c) => c.trim()).filter(Boolean),
  };
  if (form.severity.value) body.severity = form.severity.value;
  if (form.reporter.value.trim()) body.reporter = form.reporter.value.trim();
  if (form.repository.value) body.code_context = { repository: form.repository.value };
  try {
    const receipt = await api("POST", "/v1/tickets", body);
    toast(receipt.duplicate ? "That ticket already exists." : "Ticket created.", "success");
    select(receipt.id);
    await refresh(true);
  } catch (error) {
    toast(`Could not create the ticket: ${error.message}`, "error");
  }
}

// ---------------------------------------------------------------- ticket list

async function loadTickets() {
  const response = await api("GET", "/v1/tickets?limit=100");
  state.tickets = response.tickets;
  const list = document.getElementById("ticket-list");
  if (!state.tickets.length) {
    list.replaceChildren(h("li", { class: "empty" }, "No tickets yet. Create one above."));
    return;
  }
  list.replaceChildren(...state.tickets.map((t) => h("li", {},
    h("button", {
      type: "button",
      class: `ticket-row ${t.id === state.selectedId ? "selected" : ""}`,
      "aria-current": t.id === state.selectedId ? "true" : null,
      onclick: () => select(t.id),
    },
    h("span", { class: "ticket-title", text: t.title }),
    h("span", { class: "ticket-meta" },
      statusBadge(t.status),
      t.fix ? badge(`fix-${t.fix}`, FIX_LABELS[t.fix] || t.fix) : null,
      h("span", { class: "muted mono", text: shortId(t.id) })),
    ))));
}

function select(id) {
  state.selectedId = id;
  state.viewKey = "";
  if (window.location.hash !== `#${id}`) window.history.replaceState(null, "", `#${id}`);
  loadTickets().catch(() => {});
  loadView();
}

// ---------------------------------------------------------------- detail

async function loadView() {
  const detail = document.getElementById("detail");
  if (!state.selectedId) {
    detail.replaceChildren(renderIntro());
    return;
  }
  try {
    const view = await api("GET", `/v1/tickets/${state.selectedId}`);
    const key = JSON.stringify(view) + JSON.stringify(state.notes[state.selectedId] || {});
    if (key === state.viewKey) return; // unchanged: keep any half-typed form input
    state.view = view;
    state.viewKey = key;
    detail.replaceChildren(renderTicket(view));
  } catch (error) {
    if (error.status === 404) {
      state.selectedId = null;
      detail.replaceChildren(renderIntro());
    } else {
      toast(error.message, "error");
    }
  }
}

function renderIntro() {
  return h("div", { class: "card intro" },
    h("h2", {}, "How this demo works"),
    h("p", {}, "The server has a knowledge base of resolved tickets. Some of them stored the code patch that fixed them. A new ticket goes through four steps:"),
    h("ol", { class: "intro-steps" },
      h("li", {}, h("strong", {}, "Create. "), "Pick an example on the left or write your own ticket."),
      h("li", {}, h("strong", {}, "Triage. "), "The ticket is compared with past tickets, its severity is assessed, and it is auto-resolved, forwarded to L2, or escalated to an expert."),
      h("li", {}, h("strong", {}, "Fix. "), "For eligible bugs and incidents, the patch from the most similar resolved ticket is re-applied in a throwaway checkout and pushed to a review branch."),
      h("li", {}, h("strong", {}, "Resolve. "), "Record how it was really fixed. The knowledge base learns, so the next similar ticket is handled better.")),
    h("p", { class: "muted" }, "Start with the “Checkout crash on empty cart” example to see a code fix end to end."));
}

function renderTicket(view) {
  const t = view; // TicketView flattens the ticket fields
  const repo = t.code_context && t.code_context.repository;
  return h("div", { class: "ticket-detail" },
    h("div", { class: "card ticket-head" },
      h("div", { class: "ticket-head-top" },
        h("h2", { text: t.title }),
        statusBadge(t.status)),
      h("div", { class: "chips" },
        h("span", { class: "chip" }, t.kind.replace("_", " ")),
        h("span", { class: "chip" }, `client ${t.client}`),
        repo ? h("span", { class: "chip" }, `repo ${repo}`) : null,
        h("span", { class: "chip mono", title: t.id }, shortId(t.id)),
        h("span", { class: "chip" }, when(t.created_at)))),
    h("ol", { class: "steps" },
      stepReceived(view),
      stepTriage(view),
      stepFix(view),
      stepResolve(view)));
}

function step(number, title, status, description, ...body) {
  const labels = { done: "Done", current: "Next", pending: "Waiting", skipped: "Skipped", working: "Running" };
  return h("li", { class: `step step-${status}` },
    h("div", { class: "step-marker", "aria-hidden": "true" }, status === "done" ? "✓" : String(number)),
    h("div", { class: "step-body card" },
      h("div", { class: "step-head" },
        h("h3", {}, title),
        h("span", { class: `step-state state-${status}` }, labels[status])),
      description ? h("p", { class: "muted" }, description) : null,
      ...body));
}

function field(label, value) {
  if (value === null || value === undefined || value === "" || (Array.isArray(value) && !value.length)) return null;
  return [h("dt", {}, label), h("dd", {}, value)];
}

function stepReceived(view) {
  const t = view; // TicketView flattens the ticket fields
  const reporter = t.reporter
    ? [t.reporter.name, t.reporter.email].filter(Boolean).join(" · ") || t.reporter.external_id
    : null;
  return step(1, "Received", "done", "Validated and stored. Nothing has been decided yet.",
    t.description ? h("p", { class: "description", text: t.description }) : null,
    h("dl", { class: "facts" },
      field("Reporter", reporter),
      field("Components", t.components.join(", ")),
      field("Reported severity", t.reported_severity),
      field("Repository", t.code_context && t.code_context.repository),
      field("Links", t.links.length ? t.links.map((l) => l.url).join(", ") : null)));
}

function stepTriage(view) {
  const t = view; // TicketView flattens the ticket fields
  const report = view.triage;
  if (!report) {
    const auto = state.health && state.health.auto_triage;
    const running = t.status === "triaging" || (auto && t.status === "received");
    return step(2, "Triage", running ? "working" : "current",
      "Compare with past tickets, assess severity, and decide who handles it.",
      running ? h("p", {}, "Triage is running…")
        : actionButton("Run triage", () => runAction("triage", `/v1/tickets/${t.id}/triage`)));
  }
  const sev = report.classification.severity;
  return step(2, "Triage", "done", null,
    h("div", { class: "triage-grid" },
      h("div", {},
        h("h4", {}, "Severity"),
        h("p", {}, badge(`sev-${sev.severity}`, sev.severity), " ",
          h("span", { class: "muted" }, `confidence ${pct(sev.confidence)}`)),
        report.classification.category
          ? h("p", {}, h("span", { class: "muted" }, "Category "), report.classification.category) : null,
        h("ul", { class: "rationale" }, sev.rationale.map((r) => h("li", { text: r })))),
      h("div", {},
        h("h4", {}, "Most similar resolved tickets"),
        report.matches.length
          ? h("ul", { class: "matches" }, report.matches.map((m) => h("li", {},
            h("span", { class: "match-title", text: m.title }),
            meter(m.score),
            h("span", { class: "mono match-score", text: pct(m.score) }))))
          : h("p", { class: "muted" }, "The knowledge base is empty."))),
    renderDecision(report.decision));
}

// A similarity bar. The width is set through the CSSOM: the page's
// Content-Security-Policy forbids inline style attributes, but not this.
function meter(score) {
  const fill = h("span", { class: "meter-fill" });
  fill.style.width = pct(Math.max(0, Math.min(1, score)));
  return h("span", { class: "meter", title: pct(score) }, fill);
}

function renderDecision(decision) {
  const titles = {
    auto_resolve: "Auto-resolved: a known fix that is safe to apply without a human",
    forward: `Forwarded to ${decision.assignee ? decision.assignee.name : "L2"}: a known problem that a human should fix`,
    escalate: `Escalated to ${decision.assignee ? decision.assignee.name : "an expert"}`,
  };
  return h("div", { class: `decision decision-${decision.action}` },
    h("h4", {}, titles[decision.action] || decision.action),
    decision.reason ? h("p", {}, h("span", { class: "muted" }, "Why: "), decision.reason) : null,
    decision.score !== undefined ? h("p", {}, h("span", { class: "muted" }, "Similarity: "), pct(decision.score)) : null,
    decision.resolution ? h("p", { class: "description", text: decision.resolution }) : null,
    decision.suggested_resolution
      ? h("div", {}, h("span", { class: "muted" }, "Suggested fix from the past ticket:"),
        h("p", { class: "description", text: decision.suggested_resolution })) : null);
}

function stepFix(view) {
  const t = view; // TicketView flattens the ticket fields
  const fix = view.fix;
  const note = (state.notes[t.id] || {}).fix;
  const fixesOn = state.health && state.health.fixes_enabled;
  const description = "Re-apply the patch of the most similar resolved ticket in a throwaway checkout, then commit and push it to a review branch.";
  if (!view.triage) return step(3, "Code fix", "pending", description);
  if (!fixesOn && !fix) {
    return step(3, "Code fix", "skipped", "Code fixes are turned off on this server ([fixes] enabled = false).");
  }
  if (!fix) {
    if (t.status === "auto_resolved" || t.status === "resolved") {
      return step(3, "Code fix", "skipped", "This ticket is already resolved, so no code change is needed.");
    }
    if (note) {
      return step(3, "Code fix", "skipped", description, h("p", { class: "notice" }, note));
    }
    const auto = state.health && state.health.auto_triage;
    return step(3, "Code fix", "current", description,
      auto ? h("p", { class: "muted" }, "In automatic mode, eligible tickets are fixed right after triage.") : null,
      actionButton("Attempt code fix", () => runAction("fix", `/v1/tickets/${t.id}/fix`)));
  }
  const result = fix.result;
  const retry = result !== "proposed" && t.status !== "resolved"
    ? actionButton("Try again", () => runAction("fix", `/v1/tickets/${t.id}/fix`), "ghost") : null;
  return step(3, "Code fix", "done", null,
    h("p", {}, badge(`fix-${result}`, FIX_LABELS[result] || result), " ",
      h("span", { class: "muted" }, when(fix.attempted_at))),
    fix.summary ? h("p", { class: "description", text: fix.summary }) : null,
    fix.error ? h("p", { class: "notice", text: fix.error }) : null,
    result === "no_repository" ? h("p", { class: "muted" }, "No configured repository matches this ticket. Set a repository or a matching component.") : null,
    result === "proposed" ? h("dl", { class: "facts" },
      field("Repository", fix.repo),
      field("Branch", h("code", { text: fix.branch })),
      field("Commit", h("code", { text: fix.commit.slice(0, 12) })),
      field("Review", fix.change_request.url || "Push only: no code host is connected yet")) : null,
    result === "proposed" && fix.diff ? renderDiff(fix.diff) : null,
    retry);
}

function renderDiff(diff) {
  const lines = diff.replace(/\n$/, "").split("\n").map((line) => {
    let cls = "";
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ")) cls = "meta";
    else if (line.startsWith("@@")) cls = "hunk";
    else if (line.startsWith("+")) cls = "add";
    else if (line.startsWith("-")) cls = "del";
    return h("span", { class: `diff-line ${cls}`, text: line || " " });
  });
  return h("details", { class: "diff", open: true },
    h("summary", {}, "Diff against the base branch"),
    h("pre", {}, lines));
}

function stepResolve(view) {
  const t = view; // TicketView flattens the ticket fields
  const description = "Record how the ticket was really fixed. It joins the knowledge base, so the next similar ticket can match it.";
  if (t.status === "resolved") {
    return step(4, "Resolve", "done", "Resolved, and the resolution was added to the knowledge base. Create a similar ticket to see it matched.");
  }
  if (t.status === "auto_resolved") {
    return step(4, "Resolve", "done", "Closed automatically with the known fix. Nothing left to do.");
  }
  if (!view.triage) return step(4, "Resolve", "pending", description);

  const decision = view.triage.decision;
  const proposed = view.fix && view.fix.result === "proposed" ? view.fix : null;
  const summary = proposed
    ? `Merged the automatic fix on ${proposed.branch}.`
    : decision.suggested_resolution || "";
  const form = h("form", { class: "form resolve-form" },
    h("label", { class: "field" }, h("span", {}, "What fixed it"),
      h("textarea", { name: "summary", rows: "3", required: true }, summary)),
    h("label", { class: "field" }, h("span", {}, "Resolved by"),
      h("input", { name: "resolved_by", value: "demo-user" })),
    h("label", { class: "check" }, h("input", { type: "checkbox", name: "automatable" }),
      " Safe to apply automatically to future matches"),
    proposed ? h("label", { class: "check" },
      h("input", { type: "checkbox", name: "patch", checked: true }),
      " Store the proposed patch with this resolution") : null,
    h("button", { type: "submit", class: "primary" }, "Record resolution"));
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const body = {
      summary: form.summary.value.trim(),
      resolved_by: form.resolved_by.value.trim() || null,
      automatable: form.automatable.checked,
    };
    if (proposed && form.patch && form.patch.checked) body.patch = proposed.diff;
    await runAction("resolve", `/v1/tickets/${t.id}/resolution`, body);
  });
  return step(4, "Resolve", "current", description, form);
}

function actionButton(label, onClick, variant = "primary") {
  return h("button", { type: "button", class: variant, disabled: state.busy || null, onclick: onClick }, label);
}

async function runAction(kind, path, body) {
  if (state.busy) return;
  state.busy = true;
  const id = state.selectedId;
  state.viewKey = "";
  document.querySelectorAll(".step button").forEach((b) => { b.disabled = true; });
  try {
    await api("POST", path, body);
    if (state.notes[id]) delete state.notes[id][kind];
    const done = { triage: "Triage finished.", fix: "Fix step finished.", resolve: "Resolution recorded. The knowledge base learned from it." };
    toast(done[kind], "success");
  } catch (error) {
    if (kind === "fix" && error.status === 409) {
      const reason = error.message.replace(/^conflict: /, "");
      state.notes[id] = { ...(state.notes[id] || {}), fix: reason };
    } else {
      toast(error.message, "error");
    }
  } finally {
    state.busy = false;
    state.viewKey = "";
    await refresh(true);
  }
}

// ---------------------------------------------------------------- polling

async function refresh(force) {
  if (force) state.viewKey = "";
  try {
    await Promise.all([loadTickets(), loadView(), force ? loadHealth() : null]);
  } catch (error) {
    toast(error.message, "error");
  }
}

async function main() {
  await loadHealth();
  setupForm();
  const fromHash = window.location.hash.slice(1);
  if (/^[0-9a-f-]{36}$/.test(fromHash)) state.selectedId = fromHash;
  await refresh(true);
  window.addEventListener("hashchange", () => {
    const id = window.location.hash.slice(1);
    if (id && id !== state.selectedId) select(id);
  });
  setInterval(() => { if (!state.busy) refresh(false); }, 2500);
}

main();
