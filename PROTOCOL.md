# AItoCraft protocol v1

AItoCraft connects the user's own agent (Claude Desktop / Cowork, Claude Code, any MCP client)
to every document open in the ArtCraft Craft apps (GridCraft, WordCraft, DeckCraft) through **one**
MCP server entry, configured once. The user picks the context: they start a conversation where the
work belongs (a Cowork project, a plain chat, a Claude Code session) and say "we're working on
budget.xlsx". From then on, questions asked in that document reach that conversation, and only it.

Parties:

- **App**: a running Craft app. It already serves the agent layer over MCP (Streamable HTTP) on
  `127.0.0.1:<port>/mcp`. It announces itself and its open documents in the **registry**.
- **Hub** (`aitocraft`, this repo): an MCP server over stdio that the client starts. It reads the
  registry, merges the apps' tools, binds conversations to documents, and routes calls.
- **Agent**: the model in the client conversation.

The apps never talk to the hub directly; the hub is a client of the apps' MCP servers.

## 1. Registry

One file per running app instance, in a shared folder:

| OS | Folder |
| --- | --- |
| macOS | `~/Library/Application Support/ArtCraft/agents/` |
| Linux | `$XDG_DATA_HOME/ArtCraft/agents/` (default `~/.local/share/ArtCraft/agents/`) |
| Windows | `%APPDATA%\ArtCraft\agents\` |

File name `<app>-<pid>.json`. The app writes it atomically (temp file in the same folder, then
rename) at start, whenever a document opens, closes, is renamed or saved under a new path, or the
frontmost document changes (debounced, at most once a second), and as a heartbeat every 10 s.
It deletes the file on exit. Readers treat a file as stale when the pid isn't alive or the
heartbeat is older than 30 s, and may delete stale files.

```json
{
  "protocol": "aitocraft/1",
  "app": "gridcraft",
  "appTitle": "GridCraft",
  "version": "0.7.0",
  "pid": 41234,
  "mcp": "http://127.0.0.1:7980/mcp",
  "heartbeat": 1792160000000,
  "documents": [
    {
      "id": "gridcraft:41234:3",
      "title": "budget.xlsx",
      "path": "/Users/anton/Clients/Acme/budget.xlsx",
      "frontmost": true,
      "openQuestions": 2,
      "listeners": [{ "listener": "L7f3a", "label": "Cowork · Acme", "lastSeen": 1792159990000 }]
    }
  ]
}
```

- `id` is unique across apps and stable while the document stays open (`<app>:<pid>:<doc uid>`).
- `focusedAt` (optional, on a document, or on the whole file for every document of the app): when
  that document's window last came to the front. `attach` without a document picks the frontmost
  document with the latest `focusedAt`.
- `path` is null for an unsaved document. `frontmost` is true for the document in the app's
  frontmost window (at most one per app).
- Times are milliseconds since the Unix epoch.
- The registry holds no document content and no client data: titles and paths only.
- `AITOCRAFT_REGISTRY_DIR`, when set, replaces the folder (tests, isolated runs). The hub reads only
  `*.json` files, so the temp file of an atomic write can be any other name.

## 2. What every app's MCP server adds

All existing tools keep working unchanged. Every app adds:

1. **`document` on every tool** (optional string, a registry `id`). With it, the tool acts on that
   document whether or not it is active; without it, on the active document (as today). An unknown
   or closed id is an error: `document <id> is not open in <App>`.
2. **`list_documents {}`**: the same objects as in the registry's `documents`.
3. **`attach {document, label}`** → `{listener, document, title, app}`. Creates a listener for the
   document; `label` is what the user sees ("Cowork · Acme", "Claude Code · gridcraft-fork").
   **`detach {listener}`** removes it.
4. **`listener` on `list_questions`, `wait_for_questions`, `claim_question`, `answer_question`**
   (optional). A call carrying a listener refreshes its `lastSeen`. A listener not seen for 90 s is
   dropped. `wait_for_questions {document, listener}` returns only that document's questions.
5. **Questions carry their document**: every question object (tools, `/events`, channel events)
   has `document` (id) and `documentTitle`. `GET /events?document=<id>` streams only that
   document's questions. The hub also passes `&listener=<id>`; an app may treat the open stream as
   that listener being seen. Optionally, a `/events` line carries `place`: where the question was
   asked, said for a person ("Sheet1!C16 (Revenue)", "paragraph 13 (“…”)", "slide 4 · Pricing"),
   and `refs` (what ⟦n⟧ in the text points at). Without `place` the hub reads `range`/`label`,
   `paragraph`/`quote` or `where.label`.
6. **Ping** (internal only): `agent.ping` targets the active document; the ping goes to that
   document's listeners (and to every unfiltered `/events` stream). A ping answer records the
   listener that answered. The app pings quietly to check readiness (§7); the UI never shows a
   ping or its wording. An unanswered ping drops out of the question list after 60 s.
7. **Readiness per document**: the Ready lamp of the Context group (§7), from the document's
   listeners and the background client. When nobody listens, questions still queue in the document.
8. **Events besides questions** (§6) on `/events` and in `wait_for_questions` results.
9. **Off**: an app whose agent connection is off refuses agent calls with "the agent connection is
   off in <App> (…)"; the hub passes such errors to the agent word for word.

## 3. The hub

`aitocraft` speaks MCP (newline-delimited JSON-RPC 2.0) on stdin/stdout, nothing else on stdout,
and exits when stdin closes. It is installed once in the client:

- Claude Desktop / Cowork: `claude_desktop_config.json` → `"aitocraft": {"command": "<path>/aitocraft"}`.
- Claude Code: `claude mcp add --scope user aitocraft -- <path>/aitocraft`.

### Tools

- `list_documents {}`: documents of every live app, merged, each with `app` and `appTitle`.
- `attach {document?: id | name, label?}`: binds this conversation to a document. `document` may be
  a registry id, a file name or a part of it ("budget"), or omitted for the frontmost document of
  the most recently focused app. Ambiguous names are an error listing the candidates. Calls the
  app's `attach` and keeps the binding. `label` defaults to the client's name from `initialize`
  (`clientInfo.name`) plus, when given, what the agent passes (the project's name).
- `detach {document?}`.
- `app_tool {document?, tool, arguments}`: calls an app's tool by name, for clients that don't
  refresh their tool list on `list_changed` (an app started after the conversation).
- **Every tool of every live app**, merged by name. Tools with the same name in several apps
  (`get_context`, `list_questions`, `answer_question`, …) are routed by `document` (an id, or a
  name as for `attach`). If it's omitted, they go to the single attached document. With more than
  one attached, or none attached and more than one open, they return an error asking for `attach`
  or `document`; with none attached and exactly one open, they go to that one. App-specific tools (`read_range`,
  `read_document`, `read_slides`, …) go to their app. The hub adds `listener` to the calls it
  forwards (to the four question tools, and to any tool whose schema declares `listener`). Tool
  lists are fetched from the apps at start and when the registry changes, and the hub sends
  `notifications/tools/list_changed` when they change. The last known tools of each app are kept
  (`ArtCraft/aitocraft-tools.json`) and stay listed while the app isn't running, so a client that
  started first still has them; calling one then says "GridCraft isn't running".

One hub process may serve several conversations (Claude Desktop shares a server process), so
bindings are keyed by `document`. The agent keeps the document id from `attach` and passes it on
every call. The hub's instructions say so.

### Channels (Claude Code "send to my session")

The hub declares `capabilities.experimental["claude/channel"] = {}`. In a Claude Code session
started with `--dangerously-load-development-channels server:aitocraft`, after `attach` the hub
follows `GET /events?document=<id>` of that document and emits each question as
`notifications/claude/channel` with `content` (the question with its place, its topic, the pending
proposals nearby and the file context summary) and `meta` (`document`, `question_id`, `kind`,
`app`, and `topic` when the question has one). The events of §6 are passed on the same way, with
`kind` set to the event's name and `topic`/`option` in `meta`. While it follows a document it refreshes that listener every 30 s
(`list_questions {document, listener, status: "pending"}`), so the app keeps showing it as
listening. When an attached document closes, the session gets one more event with `kind: "closed"`.
Channel events are on for Claude Code (`clientInfo.name` `claude-code`) and for clients that declare
`experimental["claude/channel"]`; `AITOCRAFT_CHANNELS=0|1` forces them off or on. The same
one-time setup then serves both "work through my chat" (Cowork, `wait_for_questions`) and "push
into my Claude Code session".

### What the user says

"We're working on budget.xlsx" (or "on the open file"). The agent calls `list_documents` and
`attach`, then either waits with `wait_for_questions {document}` in a loop (Cowork, Claude Desktop)
or simply receives channel events (Claude Code with channels). The Context group's **Get ready ▸
Ask Cowork to collaborate** (§7) copies: `Work with me on "<title>" in <App> through AItoCraft:
attach to it and answer my questions from the document as they come.`

## 4. Background agent (demoted)

The app-run background agent is the Context group's **No context** mode, and the client behind its
**Claude Code ▸ Project folder** variant (§7). By default it runs in a neutral, empty folder of the
app (`ArtCraft/agent-sessions/`, its sessions grouped there in Claude Desktop) so that no project's
CLAUDE.md, memory or settings become its context. It is allowed only the document's tools
(`--strict-mcp-config`, `--allowedTools "mcp__<app>__*"`), plus WebSearch and WebFetch when the
person ticks **Web**. Running it in a project folder is an explicit choice (Project folder), whose
tooltip says that it then sees that folder's CLAUDE.md, memory and files.

## 5. Versioning

`protocol` is `aitocraft/1`. A hub ignores registry files with another major version. New fields
may be added; readers ignore what they don't know.

## 6. Topics, proposals and events (TOPICS.md v1.1)

The topic model (TOPICS.md) adds to what the apps send; the hub passes all of it through and relies
on these shapes:

- **Questions** carry `topic: {id, number, status, statusText}`, `pendingNearby: [{topic, option,
  summary}]` (pending proposals on or next to the question's places) and `fileContext` (a short
  summary of the file context, TOPICS.md §4c).
- **Events** are objects with `event` instead of a question, on `/events` (one per line) and as
  items in `wait_for_questions` results:

  ```json
  {"event": "proposal_rejected", "id": "e12", "document": "gridcraft:41234:3", "documentTitle": "budget.xlsx",
   "topic": {"id": "t7", "number": 3}, "option": "A", "title": "Spread capex over 3 years",
   "comment": "Only 2 years, the CFO said so", "hint": "the comment also comes as a follow-up question"}
  ```

  | `event` | When | The agent |
  | --- | --- | --- |
  | `withdrawn` | The person took a question back (undo). `id` is the question's. | Drops it; `answer_question` would be refused. |
  | `proposal_applied` | The person applied an option. | Nothing to do. |
  | `proposal_rejected` | The person rejected it, `comment` optional. The comment also arrives as a follow-up question in the topic. | Redoes the proposal when that question comes. |
  | `proposal_outdated` | A recorded step no longer fits the document. | Redoes the proposal with `run_command {topic}`. |

  `id` is unique per event (a question's id for `withdrawn`); the hub sends each event once.
- **Proposals**: `run_command {…, topic, option?}` records a change on the topic's draft instead of
  making it; `describe_option`, `discard_proposal`; reads mark `pending`, and `view: "proposal"`
  shows an option applied (TOPICS.md §4b).

## 7. Context: what the agent knows (the Agent tab's Context group)

The ribbon's Agent tab has a **Context** group in place of the old Connection group. It answers one
question: what does the agent know. The Mode list, the Copy ▾ menu and the status line are folded
into it and removed; everything behind them keeps working (the AItoCraft installer, the phrase,
channels, the background runner).

**Left: the mode button.** A large button showing the current mode with a status dot. Its menu has
three rows, each with the name, one line, and chips for what the agent knows:

| Mode | Line | Chips | Behind it |
| --- | --- | --- | --- |
| **No context** | "Only this document. Runs in the background." | Document | The background client (§4) in its neutral, empty folder. |
| **Claude Cowork** | "Your Cowork chat, its project and memory." | Document · Your chat | AItoCraft and the `wait_for_questions` loop in the person's chat. |
| **Claude Code** | "Your Claude Code session or a project folder." | Document · Project folder | AItoCraft with channels (My session), or the background client in a chosen folder (Project folder). |

**Right: what changes with the mode.**

- **No context**: a **☐ Web** checkbox (when on, the background client may also use WebSearch and
  WebFetch); the Ready lamp; **Get ready ▾**, shown only while the `claude` CLI isn't logged in, with
  one step, **Log in to Claude** (opens a terminal running `claude auth login`).
- **Claude Cowork**: the Ready lamp; **Get ready ▾** with **Install AItoCraft** (`aitocraft
  --install-desktop`, §3) and **Ask Cowork to collaborate** (copies the phrase, §3).
- **Claude Code**: a toggle **My session | Project folder**; the Ready lamp; **Get ready ▾**:
  - My session: **Install AItoCraft** (`claude mcp add --scope user aitocraft -- <hub path>`; done
    when `claude mcp get aitocraft` succeeds) and **Ask my session to collaborate** (copies `claude
    --dangerously-load-development-channels server:aitocraft`).
  - Project folder: **Choose folder…**, then the path is shown. The background client runs with that
    folder as its cwd; the tooltip says it then sees that folder's CLAUDE.md, memory and files.

**Rules.**

- **The Ready lamp**: green **Ready** when a listener for this document was seen recently, or the
  background client is usable; amber **Waiting** after a Get ready step, until a listener attaches;
  grey **Not ready** otherwise. The app checks by itself and pings quietly when it needs to.
- **No ping wording in the UI**, ever ("Is anyone there?" included). The ping stays an internal
  mechanism (§2.6).
- **Get ready**'s main click runs the next step not yet done; its dropdown lists all the steps,
  done ones ticked.
- **The mode is per document**, saved in the document's agent layer. A new document starts with the
  last mode used in the app.
- Mode, Web and folder changes are the person's actions: undoable where the app already makes layer
  actions undoable (TOPICS.md §4a), with keytips like the rest of the ribbon.
