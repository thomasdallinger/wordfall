# System map: choices made on this branch

One line per choice; the simplest option that fit was taken each time.

- Data: nodes (name, kind, known facts), edges (from, to, what, how), open points tied to a node, numbered O1, O2, … and never reused; stored on the meeting as `systemMap` in `meeting.json`.
- Kinds: exactly the four of the export format (our system, customer system, outside service, person or role).
- Model road: the questions' `wordfall-llm` worker (the configured notes model, Qwen3 4B by default), one process for both; no second model in memory.
- Cadence: the questions' tick (≥ 30 new words and ≥ 30 s), map first, then questions, so the questions see the latest open points.
- A change to My notes also makes the next tick due (still ≥ 30 s apart), so the map reacts to a note even in a lull.
- Input per run: the map as it stands, the notes, the line said just before, and only the talk since the last map run (capped at 6,000 characters).
- Answer form (v2, after research): numbered nodes (N1…) and numbered transcript lines (L1…); one change per line — `node N7: Name (kind) [also called X] [L2]`, `alias`, `known`, `flow N2 -> N7: what | how: … [L4]`, `open N2: …? [L6]`, `solved O3: … [L8]` — nodes first; every line cites its source ([Ln] or [notes]). The v1 block-per-node form let the model invent flows that only shared a word with the talk.
- GBNF grammar: built per run from what exists — node numbers (existing + 3 offered new ones), line numbers, open point numbers — so the model can only point at real things; at most 3 new nodes + 7 other lines; word-limited fields, words ≤ 20 letters, no CJK/full-width characters (the 4B model drifted into Chinese), optional last newline.
- "Me's system" is the kind word the model sees; the export writes "our system" (the others' "our ERP" made the model file customer systems as ours).
- Greedy decoding (temperature 0) and no repetition penalty for the map, so a run on the same talk gives the same map; the questions keep the helper's 0.4.
- Checks in code against the cited line instead of "some word was said somewhere": a new node's name is in its line (±2), a fact's content words mostly are (±1, five-letter stems) and its node is named nearby, a flow's two ends are both named within ±2 lines, an open point cites a question / someone unsure / the notes, a "solved" fact is in its line and shares a word with the open point; file formats and channels (CSV, PDF, CAMT, mail, API) and "Me" are never nodes; deleted names and numbers never come back.
- Entity resolution: "also called" / `alias` lines; a proper name takes over from a generic one ("our ERP" → Business Central, the old name kept as an alias); names are matched on whole words, not letters ("ERP" is not in "LagerPro"). The v1 extra "kinds" call is gone: flows can only point at numbered nodes.
- Prompt: fixed instructions and one worked example (with a "named together, but no flow" case) first, then the map, notes and the last 4 earlier lines plus the new ones, numbered.
- Questions: the map's open points ("Name: what is unknown") go into the question prompt as what good questions find out; grammar unchanged.
- Notes "before the meeting": there is no pre-meeting draft; notes typed right after Start count, since the map only runs once talk comes in.
- User edits: rename (old name kept as an alias for the model's later edits; a name another box has is refused) and delete (with its flows and open points); nothing else.
- Edits only while recording; afterwards the map is a read-only "System map" tab.
- Export: `## System map` appended below the notes Markdown (after Open questions) when the notes are written, and again on "Write the notes again"; that is what Copy as Markdown copies.
- Export labels stay literally as specified (System map, Known, Connects to, Open, the kind words) so other tools can parse them; names, facts, flows and open points are in the meeting's language.
- Export order: kinds in the listed order, nodes in the order they came up, a flow listed under the node it comes from, no blank lines inside the section.
- Picture: HTML boxes in a column per system kind plus a row of people, SVG lines under them; kinds told apart by border (bold = ours, dashed = outside, round = person), dashed line = how unknown; no colour beyond ink.
- Layout: while recording, the map sits above the notes/transcript tabs beside the questions; the page widens to 1160 px for it.
- Setting: Settings → Meetings → Draw a system map (`meetingMap`, on by default), like the questions' switch.
- The meeting thread waits for the live model's last run before writing the notes, and the final transcript pass no longer overwrites map or questions changed meanwhile.
- Replay: `wordfall --meeting … --map [--questions] [--note-at "90:text"] [--map-json f]` prints each map change and the cost of every run; `WORDFALL_MAP_RAW=1` also prints the model's raw answers.
- Last map run when a meeting stops, once the last seconds are transcribed (the meeting thread signals it), so the end of the talk isn't lost.
- Speed: the helper keeps one model context per job ("map", "questions") and only reads where a prompt differs from that job's last one (about 60 % of a map prompt is kept); greedy runs check only the likeliest token against the grammar and mask the whole vocabulary only when it is refused. Qwen3 4B map update 6.9 s → 3.6 s, Gemma 3 12B 19 s → 11 s, same answers.
- Settings: *Live help while you talk* (`meetingLiveHelp`) turns questions and map off together (no model process while recording); the two own switches sit under it.
- Test bench: `--save-transcript` / `--transcript` replay the live model on a saved transcript without speech recognition, `--llm <id>` picks the model per run; answer keys and a scorer for the test meetings are kept outside the repo.
- Live help uses the notes model chosen in Settings (no separate choice): one model to download and keep warm.
- More checks from the Qwen3 30B runs: a generic box ("webshop", "warehouse") named in the cited line or the one before turns into the proper name given there ("which system is that?" "a Shopware shop"); names with a format, channel or folder word ("shared folder", "bank statement file", "API access") or starting with a number are no boxes; a line that is itself unsure ("I'd have to look up whether…") solves nothing; a fact that only repeats its box's name is dropped; a "how" nobody said near the flow becomes "unknown".
- Model for live help (measured on the two test meetings, saved-transcript replay, greedy): Qwen3 30B-A3B is the best overall (English 31–32 points, German 13, map update ≈ 4 s, same as 4B); Gemma 3 12B is best in English (36–41) but weak in German (7) and ≈ 11 s per update; Qwen3 4B 20–28 / 8–11. The app keeps using the notes model chosen in Settings; on a Mac with 32 GB or more, Qwen3 30B is the one to pick.
- The helper drops llama.cpp's debug log level (a few lines per decoded token; an hour-long live meeting had grown `llm.log` by about 200 MB); load info, warnings and errors still go to `llm.log`.
