# AItoCraft

One MCP server for every document open in the ArtCraft Craft apps — GridCraft (spreadsheets),
WordCraft (documents) and DeckCraft (slide decks). Configure it once in Claude Desktop / Cowork and
in Claude Code; from then on any conversation can work on any open document.

Readers ask questions on cells, passages or slides without leaving the document. Your own agent
answers them, in the conversation *you* chose for that work: a Cowork project, a plain chat, a
Claude Code session. The context is a conscious choice. Nothing is decided behind your back.

## The one phrase

In the chat or session where the work belongs, say:

> We're working on budget.xlsx.

(or "on the open file"). The agent lists the open documents, attaches this conversation to that
one, and from then on answers the questions you ask in it:

- **Claude Desktop / Cowork**: the agent waits for questions (`wait_for_questions`) in a loop.
- **Claude Code with channels**: questions are pushed into the session as they're asked.

Several documents can be bound to several conversations at the same time. In each app, the Agent
tab's Context group has the steps ready (Get ready ▸ Install AItoCraft, Ask Cowork / my session to
collaborate), and shows a Ready lamp once a conversation is listening.

Inside a topic the agent doesn't edit the document: it proposes changes (with options when there is
a real choice), and you apply or reject them in the document. See [TOPICS.md](TOPICS.md).

## Install once

For **Codex Desktop on macOS**, see the opt-in
[shared App Server example](examples/codex-desktop/README.md). It includes chat-bound workbook
links, a global experimental skill, architecture, installation and reboot instructions. It
requires the companion GridCraft Codex context changes and does not change Claude configuration.

Build it (Rust, stable):

```sh
cargo build --release
```

Claude Desktop / Cowork:

```sh
./target/release/aitocraft --install-desktop
```

This adds `mcpServers.aitocraft` to `claude_desktop_config.json` and leaves everything else as it
is: the old file is copied aside first, a file that isn't valid JSON is never touched, and the new
one is written through a temporary file. Claude Desktop rewrites its config when it quits, so if
it's running, AItoCraft waits in the background until you quit it (Cmd+Q), then writes; open Claude
again afterwards.

Claude Code (all projects):

```sh
claude mcp add --scope user aitocraft -- /path/to/aitocraft
```

and, for questions pushed into the session (Claude Code channels, research preview):

```sh
claude --dangerously-load-development-channels server:aitocraft
```

`aitocraft --print-config` prints these lines with the right path; `aitocraft --list` shows the
documents open right now.

## How it works

Each running Craft app writes a small registry file (titles and paths only, no content) to
`~/Library/Application Support/ArtCraft/agents/` and serves its agent layer over MCP on
`127.0.0.1`. AItoCraft is a stdio MCP server that reads the registry, merges the apps' tools,
binds conversations to documents (`attach`) and routes every call to the right app and document.
The contract is in [PROTOCOL.md](PROTOCOL.md).

- One small binary, standard library threads, one dependency (`serde_json`), a hand-written HTTP
  client that only talks to loopback addresses.
- Bounded reads everywhere; it never panics on bad input; errors say what to do ("GridCraft isn't
  running", "no document named “forecast”; open: budget.xlsx (GridCraft), memo.docx (WordCraft)").
- The last known tools of each app are remembered, so a conversation started before the app still
  has them.

Environment: `AITOCRAFT_REGISTRY_DIR` (another registry folder), `AITOCRAFT_CHANNELS=0|1` (channel
events off or on).

## Development

```sh
cargo test
```

The tests run the hub against fake apps (a small MCP-over-HTTP server in the test code) and
registry files they write, and the stdio test drives the real binary over pipes. They never touch
your Claude config or the real registry.

## License

MIT OR Apache-2.0 (see [LICENSE](LICENSE)).
