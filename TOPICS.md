# Topics: what the agent layer keeps about a document (v1)

Shared model and UI for GridCraft, WordCraft and DeckCraft, agreed with Anton. Each app implements
it in its own fork (repos don't share code); the MCP surface is the same in all three, so AItoCraft
and the agent skill work everywhere.

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
