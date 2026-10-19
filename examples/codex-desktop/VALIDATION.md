# Validation record and manual acceptance

## Observed on 19 October 2026

Host: macOS 26.2 arm64, Rust/Cargo 1.99.1, system Python 3.9, Desktop 26.1007.30912,
bundled App Server 0.162.0-alpha.17.2, bundled Node 24.21.0.

The implementation was first run in a local prototype, then extracted into this configurable
example. Paths, process IDs, local chat IDs, tickets and user documents are intentionally not
included in this public contribution. No demand/customer-validation claim follows from this test.

| Check | Result / scope |
| --- | --- |
| Shared server and actual Desktop chat | Two protocol clients saw the same loaded chat; incoming questions appeared in that chat |
| New book provenance | Current chat ID saved in the XLSX; another new book did not inherit it |
| Link opening | Native macOS `gridcraft://` opened a closed/running app and the correct file; repeated opening reused the book |
| HTTP entry | Page button was exercised in the ordinary Desktop run; after the shared restart its Open endpoint was exercised through HTTP |
| Plain question | Formula B4 = B2 − B3, 100 − 60 = 40; answer returned through MCP with cell links |
| Proposal | Draft B3=70/B4=30; accepted workbook stayed at 60/40 until acceptance |
| Acceptance | Native `agent.applyProposal`, the same command as Apply, changed the accepted workbook to 70/30 |
| Rejection | Draft 90/10 rejected through native `agent.rejectProposal`; workbook stayed 70/30 |
| Rejection comment | Comment reached the same chat as a follow-up; answer acknowledged it without another proposal |
| Save/reopen | Values, formula, origin, answers and applied/rejected decisions survived |
| Question after reopen | Reached the same chat, answered from real cells |
| Delivery uniqueness | Five question messages, five unique IDs, all delivered once to the actual originating chat |
| Missing chat | A real fresh Desktop chat was used; the Agent panel showed the mismatch; origin stayed unchanged |
| Save As / worker restart | Binding and delivery identity survived; previous answered questions were not reposted |
| Global skill | Invoked from another working directory, with the caller's current thread ID |

The final live pass used native person-action APIs on a synthetic workbook. Computer Use and
browser tools reported `native pipe closed before response` in the shared-server launch; that
pass did not verify new physical button clicks. No separate model/CLI chat or fake resumed chat
was used to make the delivery pass. The installed Desktop's link allowlist rejected arbitrary
`gridcraft://` URLs in messages; this is why the HTTP landing page is also supplied.

The extracted installer has tests for another cwd, paths containing spaces, custom state,
explicit global skill installation and safe replacement. This is not a claim that a separate
physical Mac was used. The shared transport override can change with Desktop versions; repeat
the checks below on a new version.

## Reviewer acceptance

1. Build both review branches and run setup as described in README. Quit ordinary Desktop,
   start the shared launcher, choose a real local chat, and start the GridCraft launcher.
2. Prepare a disposable XLSX with B2=100, B3=60, B4=`=B2-B3`. Issue a link from that chat.
   Close the book and open the HTTP link, then press its Open button. Verify the file and
   chosen chat in Agent. Opening the link twice must not create two copies of the open book.
3. Ask on B4: "Why is profit 40? Show the formula and source cells." Check that the prompt
   appears in the chosen Desktop chat and the answer returns to the workbook with cell links.
4. In a new topic, ask to propose B3=70. Check the draft is 70/30 while the accepted values are
   60/40. Click **Apply option A**; check 70/30 and the unchanged formula.
5. In another topic, ask to propose B3=90. Check the draft is 90/10. Add "Keep 70 and 30; do not
   propose another change" and click **Reject**. Check the actual values remain 70/30, the
   rejection comment reaches the same chat, and no new proposal is created.
6. Save, close and reopen via the same link. Check the formula, 70/30, discussion and decisions.
   Ask another question and verify it reaches the same chat exactly once.
7. Restart only the worker and repeat a new question. Save As and reopen the copy. Previously
   answered questions must stay answered and must not be delivered again.
8. Exercise an explicitly unavailable target in a disposable test: the panel must explain the
   missing chat; the next question may open a real fresh chat with **Other chat** visible. It
   must not silently send into an unrelated available origin or rewrite the file's provenance.

Apply/Reject in normal file work remain the reader's decisions. Native person-action APIs in
the record above are a test-driver mechanism, not a permission granted to an answering model.

## Build/test gates

- GridCraft: `cargo xtask ci`; focused release checks are `cargo test --locked --release --lib
  -p gridcraft-agent -p gridcraft-agent-ui`.
- AItoCraft: `cargo test --locked`.
- Example: Python tests listed in README; Node modules can be syntax-checked with `node --check`;
  compile the Swift receiver without registering it to check the native source.

The PR description reports the actual gate results, including any inherited baseline failures.
