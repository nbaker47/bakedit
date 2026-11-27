---
name: code-walkthrough
description: >-
  Interactive walkthrough of one or more files or folders. Pulls each file up
  in bakedit, highlights the exact line or block being discussed without
  stealing terminal focus, reads each step aloud with bakedit's built-in
  cross-platform neural TTS, and explains it one block (or line) at a time with
  plain-English explanations and concrete justifications, waiting for
  confirmation before advancing. ONLY activate when the user explicitly invokes
  /code-walkthrough or explicitly asks to use the code-walkthrough skill — do
  not trigger automatically on general "explain ..." questions.
---
<!-- bakedit-bundled-skill -->

# /code-walkthrough

Walk through one or more files or folders interactively — pulling each file up in `bakedit`, highlighting the exact line or block being explained, and reading each step aloud via `bakedit`'s built-in cross-platform TTS, then waiting for the user before advancing.

## 1. Resolve targets and build a reading order

- Accepted inputs:
  - One or more files (`/code-walkthrough path/to/a.rs path/to/b.rs`, "explain these files").
  - One or more folders (`/code-walkthrough crates/herdr`, "explain this folder").
  - A specific file + range (`path/to/file.rs:10-80`).
- **Always inspect the actual files on disk first.** Never guess line numbers or structure from memory.
- When given a folder, list its files and skip noise (`.git`, `target`, `node_modules`, lockfiles, generated/minified assets, binaries unless specifically asked).
- Order multiple files in a logical reading order (e.g. entry point / types -> core logic -> wiring / UI / config -> tests).
- If the target has many files (>12 non-trivial files), briefly propose a focused core subset and ask whether to walk through that subset or everything. Otherwise, show a short numbered roadmap of the files on turn 1 and immediately start explaining Step 1.

## 2. Highlight in `bakedit` and read aloud on every step

Before presenting your explanation for each line or block, run a single `bakedit` command that pulls the file up, highlights the exact lines without stealing focus from the terminal, and speaks the explanation from `bakedit`'s own process (so audio is never cut off when the shell command returns):

```bash
bakedit --no-focus --highlight /abs/path/to/file.ext:START-END --speak "$SPOKEN_TEXT"
```

- For a single line, pass `/abs/path/to/file.ext:LINE` with `--highlight`.
- If the user has said `mute` / `quiet`, omit `--speak` (and run `bakedit --stop-speaking` when they mute).
- **Keep `SPOKEN_TEXT` natural:** A concise, plain-English version of **What it does** and **Why we need it** (no raw file paths, markdown, backticks, or code syntax).
- **Cross-platform neural voice built into `bakedit`:** Uses `edge-tts` (or `uvx edge-tts`) with `en-US-AndrewNeural` (override with `WALKTHROUGH_VOICE` / `WALKTHROUGH_RATE`) and automatically falls back to OS-native TTS (`say` / `spd-say` / `espeak-ng` / `powershell.exe`) if offline.
- Always pass `--no-focus` so the user's cursor stays in the terminal/chat (letting them type `next`, press `F7` for voice input, or ask questions immediately).
- If `bakedit` is not running or returns an error, continue the walkthrough normally and note it briefly once.

## 3. Pacing and granularity

- **One block (or one line) per turn.** Never dump a whole file at once.
  - Default to small, coherent logical blocks (~1–12 lines: a struct/type, a helper, a branch, a config stanza, a key call) so multi-file/folder walkthroughs stay brisk.
  - Switch to **strictly one line at a time** whenever the user asks (`line by line`, `one line at a time`, `slower`) or when explaining a dense/tricky line.
- **Wait to advance.** Stop after each step and wait for the user (`next`, `ok`, `got it`, etc.).
- **Stay on follow-ups.** If the user asks a question or says something is unclear, stay on the current block (re-highlighting a narrower sub-line if helpful), break it down in simpler plain English with a concrete example, and wait again.
- **Support quick navigation:**
  - `line by line` — step through the current block one line at a time, re-highlighting each line in `bakedit`.
  - `bigger chunks` — zoom back out to logical blocks.
  - `next file` / `skip file` — advance to the next file in the roadmap.
  - `back` — re-highlight and revisit the previous step.
  - `jump to <file or line>` — jump straight there and highlight it.
  - `mute` / `quiet` — run `bakedit --stop-speaking` and stay silent until `unmute`.
  - `repeat` — re-read the current step aloud.
- Skip purely blank lines and trivial closing braces unless they matter for scoping.

## 4. Response format (per step)

1. Brief progress + clickable file/line header following the active CLI's link format:
   - In `agy`: `**File 1/3 · Step 2** — [filename.ext:10-18](file:///abs/path/to/filename.ext#L10-L18)`
   - In Claude Code / Codex / Cursor: `**File 1/3 · Step 2** — \`path/to/filename.ext:10\` (lines 10-18)`
2. The exact highlighted line(s) in a fenced code block.
3. Two short, scannable `• ` paragraphs separated by a blank line:

• **What it does:** Plain-English explanation of what the highlighted line/block executes or declares.

• **Why we need it:** Concrete justification in this codebase — what would break or go wrong without it, and how it connects to the rest of the file/folder.

4. A short prompt at the end (e.g. `Ready for lines 19-28?` — or at the end of a file, `Ready to move to File 2/3 (foo.rs)?`).
