// Escalation inbox: what an expert team sees. Escalated tickets arrive live,
// each with the drafted preliminary resolution, ready to review and send.
// Helpers and the header come from common.js.
"use strict";

const inbox = {
  cards: new Map(), // ticket id -> card element (built once, so edits survive polling)
  arrived: new Map(), // ticket id -> time it first appeared while the page was open
  firstLoad: true,
  team: "",
  sending: false,
};

const NEW_FOR_MS = 60_000;

async function loadInbox() {
  const response = await api("GET", "/v1/tickets?status=escalated&limit=200&with_triage=true");
  const tickets = response.tickets.filter((t) => t.triage);
  const list = document.getElementById("inbox");
  const ids = new Set(tickets.map((t) => t.id));

  // Tickets resolved elsewhere leave the inbox.
  for (const [id, card] of inbox.cards) {
    if (!ids.has(id)) {
      card.remove();
      inbox.cards.delete(id);
    }
  }
  // Newest first: walk oldest to newest and prepend each new card.
  for (const ticket of [...tickets].reverse()) {
    if (inbox.cards.has(ticket.id)) continue;
    if (!inbox.firstLoad) inbox.arrived.set(ticket.id, Date.now());
    const card = renderCard(ticket);
    inbox.cards.set(ticket.id, card);
    list.prepend(card);
  }
  inbox.firstLoad = false;

  updateTeams(tickets);
  updateFreshness();
  applyFilter();
  renderSummary(tickets);
  focusFromHash();
}

function renderCard(ticket) {
  const report = ticket.triage;
  const decision = report.decision;
  const sev = report.classification.severity;
  const team = decision.assignee ? decision.assignee.name : "Unassigned";
  const reporter = ticket.reporter
    ? [ticket.reporter.name, ticket.reporter.email].filter(Boolean).join(" · ") || ticket.reporter.external_id
    : "unknown reporter";

  const editor = h("textarea", { name: "summary", rows: "10", required: true }, report.draft ? report.draft.text : "");
  const automatable = h("input", { type: "checkbox", name: "automatable" });
  const send = h("button", { type: "submit", class: "primary" }, "Send resolution");
  const form = h("form", { class: "form" },
    h("label", { class: "field" }, h("span", {}, "Resolution to send (edit the draft)"), editor),
    h("label", { class: "check" }, automatable, " Safe to apply automatically to future matches"),
    send);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (inbox.sending) return;
    inbox.sending = true;
    send.disabled = true;
    try {
      await api("POST", `/v1/tickets/${ticket.id}/resolution`, {
        summary: editor.value.trim(),
        resolved_by: team,
        automatable: automatable.checked,
      });
      toast(`Resolved "${ticket.title}". The knowledge base learned from it.`, "success");
      card.classList.add("leaving");
      setTimeout(() => loadInbox().catch(() => {}), 400);
    } catch (error) {
      toast(error.message, "error");
      send.disabled = false;
    } finally {
      inbox.sending = false;
    }
  });

  const card = h("li", { class: `inbox-card card sev-border-${sev.severity}`, id: `t-${ticket.id}`, "data-team": team },
    h("div", { class: "inbox-card-head" },
      h("div", {},
        h("h3", { text: ticket.title }),
        h("p", { class: "muted" }, `${ticket.client} · ${reporter} · `,
          h("span", { class: "arrived", "data-created": ticket.created_at }, ago(ticket.created_at)))),
      h("div", { class: "chips" },
        h("span", { class: "badge new-badge", hidden: true }, "New"),
        badge(`sev-${sev.severity}`, sev.severity),
        h("span", { class: "chip" }, team),
        report.classification.category ? h("span", { class: "chip" }, report.classification.category) : null,
        h("span", { class: "chip" }, ticket.kind.replace("_", " ")))),
    ticket.description ? h("p", { class: "description", text: ticket.description }) : null,
    h("p", {}, h("span", { class: "muted" }, "Why it was escalated: "), decision.reason),
    report.draft ? renderDraft(report.draft) : null,
    h("details", { class: "respond" },
      h("summary", {}, "Review and send the resolution"),
      form),
    h("p", { class: "card-links" },
      h("a", { href: `/#${ticket.id}` }, "Open in ticket intake"),
      h("span", { class: "muted mono" }, ` ${shortId(ticket.id)}`)));
  return card;
}

/** Marks recent arrivals and refreshes the "x ago" labels. */
function updateFreshness() {
  const now = Date.now();
  let fresh = 0;
  for (const [id, card] of inbox.cards) {
    const arrived = inbox.arrived.get(id);
    const isNew = arrived !== undefined && now - arrived < NEW_FOR_MS;
    card.classList.toggle("is-new", isNew);
    card.querySelector(".new-badge").hidden = !isNew;
    if (isNew) fresh += 1;
    const label = card.querySelector(".arrived");
    label.textContent = ago(label.dataset.created);
  }
  document.title = fresh ? `(${fresh} new) Escalation Inbox` : "Escalation Inbox";
}

function updateTeams(tickets) {
  const select = document.getElementById("team-filter");
  const counts = new Map();
  for (const t of tickets) {
    const team = t.triage.decision.assignee ? t.triage.decision.assignee.name : "Unassigned";
    counts.set(team, (counts.get(team) || 0) + 1);
  }
  if (inbox.team && !counts.has(inbox.team)) counts.set(inbox.team, 0);
  const teams = [...counts.keys()].sort();
  select.replaceChildren(
    h("option", { value: "" }, `All teams (${tickets.length})`),
    ...teams.map((team) => h("option", { value: team }, `${team} (${counts.get(team)})`)));
  select.value = inbox.team;
}

function applyFilter() {
  for (const card of inbox.cards.values()) {
    card.hidden = Boolean(inbox.team) && card.dataset.team !== inbox.team;
  }
}

function renderSummary(tickets) {
  const summary = document.getElementById("inbox-summary");
  const list = document.getElementById("inbox");
  const empty = list.querySelector(".inbox-empty");
  if (!tickets.length) {
    summary.textContent = "Nothing escalated right now.";
    if (!empty) {
      list.append(h("li", { class: "inbox-empty card" },
        h("h3", {}, "No escalations"),
        h("p", { class: "muted" },
          "Severe or novel tickets land here with a drafted first response. On the ",
          h("a", { href: "/" }, "ticket intake"),
          " page, switch to Automatic mode and create the “Suspicious admin logins” or “Production outage” example, or press “Simulate incoming tickets”. Then keep this page open and watch them arrive.")));
    }
    return;
  }
  if (empty) empty.remove();
  const critical = tickets.filter((t) => t.triage.classification.severity.severity === "critical").length;
  summary.textContent = `${tickets.length} escalated ticket${tickets.length === 1 ? "" : "s"}` +
    (critical ? `, ${critical} critical` : "") + ". Each comes with a drafted first response for the expert to review.";
}

let focused = "";
/** /inbox#<ticket-id> scrolls to and highlights that ticket once. */
function focusFromHash() {
  const id = window.location.hash.slice(1);
  if (!id || id === focused) return;
  const card = inbox.cards.get(id);
  if (!card) return;
  focused = id;
  card.scrollIntoView({ block: "start" });
  card.classList.add("focused");
  card.querySelector("details.respond").open = true;
}

async function refresh() {
  try {
    await Promise.all([loadInbox(), loadEscalationCount()]);
  } catch (error) {
    toast(error.message, "error");
  }
}

async function main() {
  document.getElementById("team-filter").addEventListener("change", (event) => {
    inbox.team = event.target.value;
    applyFilter();
  });
  window.addEventListener("hashchange", () => { focused = ""; focusFromHash(); });
  await loadServerInfo();
  await refresh();
  setInterval(() => {
    if (!inbox.sending) refresh();
  }, 2000);
  setInterval(() => { if (!document.hidden) loadServerInfo(); }, 10_000);
}

main();
