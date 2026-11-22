# Reddit Research (Windows desktop)

A small native Windows app for topic research on Reddit. It has a UI, uses one portable
`.exe`, and needs no browser, Python, or runtime. You can build a Windows installer for it.

## What it does

- Turn a research question into a search plan with DeepSeek or another AI model.
- Run that plan: search posts and comments across all of Reddit with the PullPush
  archive, then search the plan subreddits in the Arctic Shift archive.
- Expand the best thread for each plan question and store the parent text for context.
- Optionally use the official Reddit Data API with one account you own.
- Remove duplicate records, rank the rest, and drop bot and removed noise.
- Give each record a rank score: 45% question match, 20% engagement, 15% recency,
  10% source trust, 10% text depth.
- Keep the evidence pack balanced: cap each subreddit, each thread, and each author,
  and keep one dissenting record when one exists.
- Check each plan question against local coverage rules. Ask the AI model for gaps only
  when a rule fails. Run a new collection round only when it returns new queries.
- Show live counts for each stage: plan, collect, rank, review, write, validate, export.
- Write a concise cited report with the AI model, then check every quote against the stored text.
- Show the report in a Markdown preview inside the app, with links built only from evidence.
- Fall back to the exact-excerpt draft when no AI key is set or the AI call fails.
- Save the plan, final report, draft report, JSON, CSV, and evidence HTML in one run folder.

## Design

See [docs/design.md](docs/design.md) for the automatic research agent design:
AI query planning, multi-source evidence, citation checks, and report synthesis.
DeepSeek is the default AI provider. OpenAI, Anthropic, Gemini, and local Ollama
are also supported. The PullPush archive gives global keyword search with no
account. Free SearXNG discovery finds Reddit thread links without an API key.

## Install on Windows

1. Run `RedditResearch-Setup-1.0.0.exe`. The setup file is in `dist\` after a build.
2. Choose **Install for me only** or **Install for all users**, then finish the wizard.
3. The installer adds a Start Menu shortcut. It can also add a desktop shortcut.
4. Remove the app from **Settings > Apps > Installed apps**. The uninstaller asks before it
   deletes your saved settings and runs. AI keys in Windows Credential Manager stay.

Windows SmartScreen may warn because the setup file is unsigned. Choose **More info**, then
**Run anyway**. The installer is 64-bit and needs Windows 10 or later.

## Run it

1. Install the app, or build it and copy the exe anywhere you like.
2. Double-click it.
3. Type a research question. Press **Build research plan** to see the AI search plan.
4. Choose sources, then press **Run research**. Each stage shows a live count.
5. Read the cited report on the **Report** tab. Use **Excerpts** to see the exact-excerpt draft.
6. Open **Evidence** to check the ranked list, or **Open evidence HTML** for the full page.
7. The app loads the most recent saved run at startup. Press **Load last run** to reload it.

No account is needed for the all-Reddit pass or the archive tactics. Official tactics
stay disabled until you fill in the account fields.

## The window

- Top bar: **Runs**, **Log**, **Settings**, and the light or dark theme switch.
  **Log** opens the live activity log for this session.
- Left panel: type a research question, then press **Build research plan** or **Run research**.
- A question can be a full sentence. The app drops question words and searches the main keywords.
- Settings tabs: subreddits, limits, model, and account.
- Main tabs: **Report**, **Evidence**, **Plan**, and **History**.
- Report: the cited report in a Markdown preview. Headings, bullets, quotes, and Reddit
  links render in place. **Excerpts** shows the extractive draft with the plan question,
  the count, and exact excerpts.
- Evidence: ranked cards. Each card shows its rank number, rank score, question match and
  plan question tags, and the text it replies to. Click a card to open the full text in a
  drawer. The drawer keeps the author's line breaks, quotes, lists, and text styles. Search,
  pick a subreddit, or filter posts and comments.
- History: every saved search on this computer, newest first. Each row shows the time,
  the question, the record count, and the report type. Click a run to load its report,
  evidence, plan, and log. The log of the loaded run shows below the list. **Open runs
  folder** shows the files on disk. The top bar **Log** button opens the live activity log.

## Search scope

Every run searches all of Reddit. The planner also picks 3 to 10 seed subreddits.
The seeds are extra targets, not a limit. The app runs a global keyword search on
the PullPush archive, then a subreddit-scoped search on the Arctic Shift archive.
The official Reddit API adds fresh results when an account is connected. The
**All Reddit** chip in the left panel turns the global pass on or off. Its call
interval is on the **Limits** tab.

PullPush is a public Reddit archive. It needs no account. Use it with the same
care as the official API: one copy of the app at a time, and a slow call rate.

## Reddit account (optional)

1. Open <https://www.reddit.com/prefs/apps> and create an app of type **script**.
2. Copy the client ID and secret into the app.
3. Add your username and password, then press **Test login**.

The password stays in memory for the session only. It is never written to disk.
Use one account only. Do not create extra accounts to get around rate limits.

## AI model (DeepSeek by default)

1. Open **Settings**, then the **Model** tab.
2. Keep **DeepSeek**, or pick OpenAI, Anthropic, Gemini, or Ollama.
3. Paste your API key. Ollama needs no key.
4. Press **Test model**.
5. Use **Critic rounds** to set how many extra collection rounds the review may run.

The key is stored in Windows Credential Manager under `RedditResearch/llm/<provider>`.
It is never written to `config.json`. An empty key removes the stored value.

Press **Build research plan** to turn the question into sub-questions, subreddits, and
short search queries. With no key, or if the call fails, the app builds a local plan and
shows a note.

**Run research** uses that plan. First it searches all of Reddit with the plan queries.
Then it searches each seed subreddit. It ranks and groups the records under the plan
questions. At the end, the app builds an evidence pack and asks the model for a report
with a direct answer, themed findings, and exact quotes. It checks each quote against
the stored text before it shows the report. Read the linked threads before you act.

## Final report (Write and Validate)

The Write stage packs up to 100 records and 48,000 characters, balanced across the plan
questions. The model returns a report JSON with a summary, 3 to 5 sections, claims,
exact quotes, disagreements, limits, and open questions.

The Validate stage checks every evidence ID and every quote. A quote must be an exact
substring of the record after whitespace normalization, from 8 to 400 characters. A
claim with no exact quote is removed. If more than 20% of claims fail, the app makes one
repair call. If that also fails, the exact-excerpt draft becomes the report.

The app writes all links. Model text cannot add a URL or a link. The Sources list uses
only the stored Reddit permalinks. The report is saved as `report.md`. The Report tab
renders that Markdown with a built-in previewer. Use **Copy Markdown** or **Open
report.md**.

Your question and the Reddit text you collect go to the provider you choose. Ollama keeps
them on this computer. Do not send private data.

## Critic loop (Review stage)

After collection, the app checks each plan question with local rules:

- 5 or more different threads.
- 2 or more subreddits, when the plan lists 2 or more.
- A time span of 180 days or more.
- For opinion questions, at least one positive report and one negative report.
- At least one first-hand report (I, my, me, we).

If a rule fails, the app sends a short summary to the AI model: counts, top thread
titles, and the unmet rules. The model returns new short queries for real gaps. The app
runs them on the archive, and on the official API when it is on. Each round may use at
most 40% of the remaining request budget. The default is 2 extra rounds.

The loop stops when all rules pass, the model says stop, no new query remains, the
request budget is low, or you press **Stop**. The Review stage note shows the result.
`run.json` has a `review` block with the rounds, the coverage count, the new queries,
and the stop reason. Set **Critic rounds** to 0 on the Model tab to turn the loop off.

## Safety defaults

- 45 official requests per minute (Reddit's free limit is 100 per minute averaged over 10 minutes).
- 1.5 seconds between Arctic Shift calls.
- 2.0 seconds between PullPush calls.
- Request budget stops long runs.
- Retry with backoff on 429 and 5xx. Stop on bad login.
- One copy of the app at a time. Two runs at once would share the same account quota.
- Public content only. Private and restricted content stays out of reach unless your own
  account can already read it.
- Data belongs to its authors. Use it for personal research. Delete stored data if a
  user asks.

## Where files go

- Settings: `%APPDATA%\RedditResearch\config.json`
- AI keys: Windows Credential Manager, target `RedditResearch/llm/<provider>`
- Runs: `%APPDATA%\RedditResearch\runs\<time>-<topic>\`
  - `results.json`, `results.csv`, `report.html`, `cache.json`
  - `plan.json`, `evidence.json`, `report.md`, `report.json`, `run.log`
  - `draft-report.json`, `draft-report.md`, `run.json`
  - `run.json` and `evidence.json` hold a `review` block: rounds, coverage, new queries,
    stop reason, and the per-question unmet rules.
  - `run.json` holds a `synthesis` block: model, claims, exact quotes, linked sources,
    and the error when the app used the extractive fallback.
- Plan preview: `%APPDATA%\RedditResearch\plan-preview.json` and `plan-preview.log`
- Selftest log: `%APPDATA%\RedditResearch\selftest.log`

Delete a run folder to remove its data. Delete `config.json` to reset settings.

## Build from source

Needs Rust 1.92 or later with the MSVC toolchain.

```
cargo build --release
```

The exe is at `target\release\reddit-research-ui.exe`. It is self-contained apart from the
normal Windows system DLLs. The build embeds `assets\app.ico` and the version block.

## Build the installer

Needs Inno Setup 6. Run `cargo build --release`, then run:

```
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\RedditResearch.iss
```

The setup file is `dist\RedditResearch-Setup-1.0.0.exe`.

## Plan preview (no window)

```
reddit-research-ui.exe --plan-only "does armodafinil help with learning?"
```

The app writes `plan-preview.json` and `plan-preview.log` in `%APPDATA%\RedditResearch`.
Exit 0 means an AI plan, or a local fallback with no key. Exit 3 means the AI call failed
and a local plan was used. Exit 2 means a bad query or a write error.

## Collect only (no window)

```
reddit-research-ui.exe --collect-only "does armodafinil help with learning?"
```

The app builds the plan, runs all stages, writes the run folder, and exits. Exit 0 means
no error and at least one record. The console log is `collect-preview.log`.

## Selftest

```
reddit-research-ui.exe --selftest "adderall"
```

Exit code 0 means records were collected. Details go to `selftest.log`. The release
build has no console window, so read the log file.

## Notes

- Windows SmartScreen may warn on an unsigned exe. Choose **More info**, then **Run anyway**.
- Some antivirus tools flag small unsigned network tools. Add an exclusion only if you trust the file.
- The UI uses native OpenGL through `eframe`. It does not use WebView2 or a browser engine.
- Large subreddits can reject keyword searches as "too heavy". The app then samples recent
  rows and filters them on your computer. This is slower but keeps the run alive.
