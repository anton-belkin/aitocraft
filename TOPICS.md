# Topics: what the agent layer keeps about a document (v1.1)

Shared model and UI for GridCraft, WordCraft and DeckCraft, agreed with Anton. Each app implements
it in its own fork (repos don't share code); the MCP surface is the same in all three, so AItoCraft
and the agent skill work everywhere.

v1.1 (agreed with Anton on 2026-10-16) adds the pane's highlights (§3a), proposals instead of direct
edits (§4b) and file context (§4c).

## 1. The model

The unit is a **topic**: one question or theme about the document. A topic has:

| Part | What it is | Kept when the document is sent? |
| --- | --- | --- |
| **Places** | Where in the document it applies: cells or ranges (GridCraft), passages (WordCraft), shapes (DeckCraft). One or many. | yes |
| **Status** | One of the statuses below, plus free text ("agreed with the CFO on 14.10, if capex is confirmed"), who set it, when. | yes |
| **Notes** | Several, each addressable: explanations, sources (URL + summary), assumptions, decisions. Each has an id and a title. | yes |
| **Discussion** | The chat about this topic: questions, answers, follow-ups, in order. | no (only with "include discussion") |

The chat matters in the moment; afterwards what counts is the status and the notes, which fix what
the author wants to say about these places. So the discussion is folded away once a status is set,
and it is left out when the document is sent (web export, share), unless the sender ticks
"include discussion".

### Statuses

| Status | Meaning | Colour |
| --- | --- | --- |
| **Open** | A question or doubt, no decision yet. Set automatically by the first question. | orange |
| **Revise** | Decided that something here must change. | red |
| **Agreed** | Settled and accepted. | green |
| **For information** | Nothing to decide: an explanation, a source, a caveat. | grey |

While the agent is working on a question in the topic, the marker shows that (blue, pulsing)
regardless of status. An answer not yet read shows as a dot on the marker.

**Only a person sets Agreed.** The agent may set Open or Revise, or propose Agreed with its
reasons ("agent suggests: Agreed"), which the person confirms with one click. Every status change
is in the audit log with who made it.

### Places and numbering

- Topics are numbered by the position of their first place in the document (like footnotes:
  they renumber when a topic appears earlier). Places within a topic are numbered in document order:
  topic **3** with three cells shows **3.1**, **3.2**, **3.3**.
- A topic created from a voice question gets one place per pointed cell or passage (the ⟦n⟧ refs),
  plus the main one. A typed question gets the selection as its place.
- **The agent adds places** when it finds that a change here implies changes elsewhere: it attaches
  those places to the same topic, each with a short reason ("depends on 3.1 via the margin
  formula"), so the dependency is visible on the document itself.
- One place can belong to several topics: it then shows several markers.

### Markers in the document

A small circle with the number (**3** for a one-place topic, **3.2** for the second place of a
multi-place topic), coloured by the topic's status. Nothing else on the page: no "answered" labels.
In GridCraft the circle sits in the cell's top-right corner and is small enough not to cover the
value; several topics on one cell stack as several circles. Clicking a marker, or anywhere in a
place, opens the topic in the pane.

## 2. Links inside text

Answers, notes and status text carry **links inside the text**, not a list of links below it.

Syntax (Markdown links, the same in all apps; the agent writes them, the UI renders them):

| Link | Example | Opens |
| --- | --- | --- |
| a cell or range | `[B1](cell:Sheet1!B1)` | selects it |
| a passage | `[§12](para:12)`, `[the fee clause](quote:12/twelve months)` | scrolls to it, highlights the quote |
| a slide or shape | `[slide 4](slide:4)`, `[the chart](shape:4/k4f2a9)` | goes there |
| a topic or one of its places | `[topic 3](topic:3)`, `[3.2](place:3.2)` | opens the topic, highlights the place |
| a note | `[the source](note:3/n2)` | opens the topic on that note |
| the web | `[NOAA](https://…)` | the browser; the note's summary is the tooltip |

Plain references are linked automatically when they are unambiguous: in GridCraft `B1`, `C3:D9`,
`Sheet2!B4`; in WordCraft `§12`; in DeckCraft `slide 4`. Topic and note ids are stable; topic
*numbers* in a link are resolved when shown, so they follow renumbering.

## 2a. Stable places and links

Every place and every in-text link points at an **anchor**, never at a bare address, so it survives
editing and a round trip through Excel, Word or PowerPoint.

- **Anchors.** GridCraft: hidden defined names (`_GcAgent_<id>`, as today); they move with
  inserted and deleted rows and columns and with cut and paste. WordCraft: bookmarks in the
  document (`w:bookmarkStart/End`, named `_GcPlace_<id>`). DeckCraft: the shape's stable id (plus a
  character range for text inside a shape).
- **Links become anchors when the text is stored.** When the agent writes `[B1](cell:Sheet1!B1)`
  or a text contains a plain `B1` / `§12` / `slide 4`, the app creates (or reuses) an anchor and
  stores the link as `anchor:<id>`. A label that was the address is shown as the current address,
  so after a row insert the answer reads `B2` and still leads to the same cell.
- **Every rendered text follows the anchors silently.** Answers in the discussion, notes, status
  text and questions are stored with anchors and shown with current addresses: insert a row and a
  past answer that said `C4` now says `C5`, with nothing marking the change. The same for labels
  of places.
- **Ranges, and the agent widening them.** A place or link can be a cell or a range (GridCraft), a
  passage of any length (WordCraft), several shapes (DeckCraft). When the agent sees that an answer
  concerns more than what was selected (the whole row, the whole table, the whole clause), it widens
  the place with `widen_place {place, to}` and says why. The person sees the wider outline.
- **Deleted content keeps its whereabouts.** When the content of a place or link is deleted, the
  app keeps where it was instead of losing it. Just before the delete (an observer on the deleting
  commands), it anchors the neighbours. GridCraft: the cells just before and after the deleted
  rows or columns, as hidden names. WordCraft: a collapsed bookmark at the deletion point, plus the
  deleted text kept in the layer. DeckCraft: the slide and the position. The place then reads
  "removed — was between [B2](…) and [B3](…)" (or "removed here: “twelve months”"), with live
  links that keep following later edits. It is struck through in the topic, never silently gone
  and never pointing at the wrong thing. Undo of the delete brings the place back as it was.
- **Copy and cut.**
  - WordCraft: **cut and paste moves** the bookmark (the topic follows the text); **copy and paste
    duplicates** it as a new bookmark of the same topic, so the topic gains a place (the clause now
    exists twice and the note applies to both). Word itself drops duplicate bookmark names on
    copy; ours are distinct names, so the file keeps both.
  - DeckCraft: duplicating a shape or a slide duplicates its places in the same way.
  - GridCraft: **cut moves** the place (as Excel's names do); **copy and fill don't duplicate**
    places, because copying and filling formulas is mechanical and would flood topics with places.

## 3. The pane

Three tabs:

- **Selection** (the default): what the current selection is about.
  - What is selected: the cell and its value and formula, or the quote, or the shape.
  - The topics touching the selection, each as a card, top to bottom: number and status (with its
    text), places (each a link, with the agent's reason for places it added), notes (titles that
    expand), then **Discussion**, folded when a status is set.
  - GridCraft adds, folded below the cards: where the number comes from, sensitivity,
    alternatives, risk matrix.
  - **The question box at the very bottom.** Asking on a selection that already has a topic adds
    to that topic. "New topic" next to the box starts a separate one. A follow-up reaches the agent
    with the topic's status, notes and discussion so far.
- **Overview**: every topic of the document in document order, with its number, status, status
  text and counts (places, notes, unread answers). Filters: Open, Revise, Agreed, For information,
  unread, with the agent. Clicking a topic opens it. On top, readiness for sign-off: "Agreed 10 of
  15 · 2 revise · 3 open". It is also shown on the ribbon's Agent button.
- **History**: the chronological log of decisions and changes, as today.

The Questions and Notes tabs go away.

## 3a. Highlights and the pane (v1.1)

- **The caret highlights its topic.** When the caret (WordCraft), the active cell (GridCraft) or the
  selected shape (DeckCraft) is inside a place, that place's topic lights up: its markers, the
  place's outline, and its card in the pane.
- **Hovering a link highlights its target.** Hovering a link in the pane (an answer, a note, a
  place) highlights what it points at, without moving focus or the selection. A target that is off
  screen gets an edge hint in its direction ("↓ §12", "→ F40"). A removed place shows its "was
  between" neighbours highlighted.
- **File context** has a button beside the pane's tabs (§4c).
- The pane's font is one point larger than the app's interface font.

## 4. MCP for the agent

All tools take `document` (AItoCraft). New or changed:

- `list_topics {status?}` → topics with places (resolved), status, note titles, unread flags.
- `get_topic {topic}` → everything, discussion included.
- `answer_question {id, text}`: `text` may contain links (§2). The `links` parameter stays
  accepted; it is appended as a sentence of links.
- `add_note {topic, title, kind, text}` → note id. Kinds: explanation, source (with `url`, `summary`),
  assumption, decision.
- `add_places {topic, places: [{range | paragraph+quote | slide+shape, reason}]}`.
- `widen_place {place, to: range | paragraph+quote | shapes, reason}`: widen a place to what the
  answer is really about (a whole row, table, clause).
- `set_status {topic, status: open | revise | info, text}`, and `propose_agreed {topic, reasons}`.
  Agreed by the agent is refused.
- Questions in `wait_for_questions` / events carry `topic` (id and number) and the topic's status,
  so a follow-up is answered in context.

The skill gets four lines: link inside the text; add places that a change here affects; leave
notes that will still matter after the chat; never mark Agreed, propose it.

## 4a. Undo

Everything the **person** does with the agent layer is undoable with ⌘Z, in the same history as
their edits and in order. The **agent's** contributions are not on the person's undo stack.

- **Mechanism.** A generic engine hook (upstreamable): custom undo steps with a label, `undo` and
  `redo`, pushed into the document's ordinary history beside its snapshot steps. Layer actions push
  such a step. They don't snapshot the document, so undoing a cell edit still never touches the
  layer, and a layer step never restores cells.
- **Undoable person actions:** mark and unmark client data, ask (typed or by voice), a new topic or
  place, set status, pin a note, delete a note, reject an agent's place, switch an alternative,
  safe file on and off, input ranges.
- **Undo of a question:**
  - Not yet taken by the agent: the question and its places disappear.
  - Being worked on: it's withdrawn. The agent gets a `withdrawn` event, and `answer_question`
    returns "this question was withdrawn by the user; drop it".
  - Still transcribing (voice): the transcription is cancelled.
  - Redo brings the question and its places back and **sends it again**.
  - Already answered: undo hides the topic with its answer. Redo restores it as it was, answer
    included, without asking the agent again.
- **Not on the person's stack:** the agent's answers, notes, added places, proposed statuses. Each
  has "remove" or "reject" in the UI, and that click is the person's action, so it is undoable.
  Cell edits the agent made through `run_command` are document edits and stay in the ordinary
  history, labelled "Undo (agent): …".
- WordCraft and DeckCraft do the same through their own undo models.
- Applying a proposal (§4b) is the person's action: one undo step for all its changes.

## 4b. Proposals instead of direct edits (v1.1)

Inside a topic the agent doesn't change the document: it proposes, and the person applies or
rejects in the document. The MCP surface is the same in all three apps.

- **`run_command {…, topic, option?}`.** With `topic`, the command doesn't touch the document: it
  runs on the topic's draft (a scratch copy) and is recorded in the proposal. `option` defaults to
  `"A"`; the agent offers more options (`"B"`, `"C"`) only when there is a real choice. Without
  `topic`, `run_command` works as before (outside any topic).
- **`describe_option {topic, option, title, why}`** names an option and says why.
  **`discard_proposal {topic, option?}`** drops one option, or the whole proposal.
- **Reads mark what a pending proposal touches.** Paragraphs, cells and shapes carry
  `pending: [{topic, option, summary}]`.
- **Reads can show the proposal.** WordCraft `read_document {view: "proposal", topic, option?}`,
  GridCraft `read_range {view: "proposal", topic, option?}`, DeckCraft `read_slides {view:
  "proposal", topic, option?}`: the document as it would be with that option applied.
- **`list_topics` and `get_topic`** show `proposal: {options: [{option, title, why, changes}],
  state}`, where `state` is `pending`, `applied`, `rejected` or `outdated`.
- **Questions and events carry `pendingNearby`**: the pending proposals on or next to the
  question's places (`[{topic, option, summary}]`), so the agent doesn't propose against them blind.
- **Events** (PROTOCOL.md §6), on `/events`, in `wait_for_questions` and as channel events:
  - `proposal_applied {topic, option}`: the person applied it. Nothing to do.
  - `proposal_rejected {topic, option, comment}`: the person rejected it. The comment also arrives as
    a follow-up question in the topic, where the agent redoes the proposal.
  - `proposal_outdated {topic, option}`: the document changed under it; ask to redo.

The UI:

- The topic card says **"The agent proposes N changes"**, shows the option tabs (A, B, …, each with
  its title and why), then **Apply option X** and **Reject** (Reject takes an optional comment).
- Switching options switches the preview in the document:
  - WordCraft: insertions, deletions and "formatting changed" on the affected paragraphs; new
    tables shown as they would be.
  - GridCraft: old → new on the changed cells, like the heat map (the draft is the recalculated
    scratch copy).
  - DeckCraft: the changed shapes outlined, with their new text.
- **Apply** replays the recorded commands as **one undo step**, the person's action, and adds a
  decision note "chose X: <title>" to the topic.
- If a recorded step doesn't fit any more (the document changed), nothing is applied: the card
  says "out of date, ask the agent to redo" and the agent gets `proposal_outdated`.

The skill and the hub's instructions say: in a topic, propose with `run_command {topic}`; never ask
for confirmation in the chat (the person applies in the document); offer options when there is a
real choice.

## 4c. File context (v1.1)

What the author wants every agent to know about the whole file: its purpose, audience, conventions,
what is settled. Entries have an id, a title, a kind and text.

- MCP: `get_file_context {document}`; `add_file_context {document, title, kind, text}` → id;
  `update_file_context {document, id, title?, text?}`; `remove_file_context {document, id}`.
- `get_context` and every question carry a short summary of it (`fileContext`).
- It is kept in the file and kept when the document is sent.
- The **File context** button beside the pane's tabs opens it. The person's edits there are
  undoable (§4a).

## 5. Files

The layer's format moves to version 2: questions become topics (one topic per existing question,
its anchor as the place, its answer in the discussion, status Open, or For information when
answered and read); existing notes become notes of a topic of their own (status For information).
Version 1 files keep loading.

## 6. Later (recorded, not now)

- **Nested places**: a smaller place inside a larger one, hierarchical numbering (3 › 3.1),
  breadcrumbs, a "New place here" button inside an existing place, status rolling up (the large
  one can't be Agreed while a nested one is Revise), the large one's status and notes passed down
  as context. Partial overlaps would be plain neighbours. Anton wants to think the relation through
  first.
