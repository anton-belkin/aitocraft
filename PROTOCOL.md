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
6. **Ping**: `agent.ping` targets the active document; the ping goes to that document's listeners
   (and to every unfiltered `/events` stream). A ping answer records the listener that answered.
7. **Status per document** (the Connection group): the active document's listeners, shown as
   "● Listening: Cowork · Acme · 20 s ago", the ping result, or "○ Nobody is listening for this
   document". When nobody listens, questions still queue in the document.

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
`notifications/claude/channel` with `content` (the question with its place) and `meta` (`document`,
`question_id`, `kind`, `app`). While it follows a document it refreshes that listener every 30 s
(`list_questions {document, listener, status: "pending"}`), so the app keeps showing it as
listening. When an attached document closes, the session gets one more event with `kind: "closed"`.
Channel events are on for Claude Code (`clientInfo.name` `claude-code`) and for clients that declare
`experimental["claude/channel"]`; `AITOCRAFT_CHANNELS=0|1` forces them off or on. The same one-time setup then serves both "work through my chat" (Cowork,
`wait_for_questions`) and "push into my Claude Code session".

### What the user says

"We're working on budget.xlsx" (or "on the open file"). The agent calls `list_documents` and
`attach`, then either waits with `wait_for_questions {document}` in a loop (Cowork, Claude Desktop)
or simply receives channel events (Claude Code with channels). Each app's Agent tab has a
"Copy phrase for Claude" button that copies: `Work with me on "<title>" in <App> through AItoCraft:
attach to it and answer my questions from the document as they come.`

## 4. Background agent (demoted)

The app-run background agent stays available as the last mode. By default it runs in a neutral,
empty folder of the app (`ArtCraft/agent-sessions/`, its sessions grouped there in Claude Desktop)
so that no project's CLAUDE.md, memory or settings become its context. It is allowed only the
document's tools (`--strict-mcp-config`, `--allowedTools "mcp__<app>__*"`). Using the document's
folder as its project is an explicit option, labelled with what it implies. The mode's line in
the list says: "The app runs Claude on your subscription. It sees only this document. Uses your
limits."

## 5. Versioning

`protocol` is `aitocraft/1`. A hub ignores registry files with another major version. New fields
may be added; readers ignore what they don't know.
