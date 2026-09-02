# Configuration

The config keys, where each one comes from, and the rules that
resolve them. Demerzel resolves config once at boot. The engine and
the tools never read config files.

## Files

1. Flags on the command line.
2. Environment variables.
3. The project file: `.demerzel/config.toml` in the working
   directory. There is no upward search.
4. The user file: `$XDG_CONFIG_HOME/demerzel/config.toml`.
5. Built-in defaults.

A higher layer wins. `demerzel doctor` prints each value with the
layer that set it.

## Keys

All keys are optional.

```toml
provider = "openai-codex"        # or "anthropic"
model = "gpt-5.6-sol"
approval = "ask"                 # read-only | ask | auto
effort = "high"                  # none … ultra, see Reasoning effort
max_model_calls = 128            # model calls one turn may make
history_file = true              # file-backed input history

[providers.openai-codex]
base_url = "https://chatgpt.com/backend-api/codex"
model = "gpt-5.6-sol"
effort = "medium"                # per-provider override

[providers.anthropic]
base_url = "https://api.anthropic.com"
model = "claude-sonnet-5"
context_window = 200000          # in tokens; wins over the model catalog

[tui]
mouse = true                     # off: the terminal's own selection, no wheel
markdown = true                  # render model replies as markdown
thinking = "closed"              # closed | open | hidden
theme = "terminal"               # terminal | dark | light | a [theme.<name>] table

[theme.ocean]                    # a theme of your own, see Themes
accent = "#5fafd7"
error = "bright-red"
```

| Key | Flag | Variable | Default |
|---|---|---|---|
| `provider` | `--provider` | `DEMERZEL_PROVIDER` | `openai-codex` |
| `model` | `--model` | `DEMERZEL_MODEL` | the provider's default |
| `base_url` | `--base-url` | `DEMERZEL_BASE_URL` | the provider's endpoint |
| `approval` | `--auto`, `--read-only` | — | `ask`; one-shot runs read-only |
| `effort` | `--effort` | `DEMERZEL_EFFORT` | `high` |
| `max_model_calls` | — | — | `128` |
| `context_window` | — | — | the provider catalog |
| `history_file` | — | — | `true` |
| `tui.mouse` | — | — | `true` |
| `tui.markdown` | — | — | `true` |
| `tui.thinking` | — | — | `closed` |
| `tui.theme` | — | — | `terminal` |
| — | — | `DEMERZEL_LOG` | unset: no trace |

In one layer, a `[providers.<id>]` value is more specific than the
top-level key, so it wins. A top-level `model` in a layer that names
another provider does not apply: a model id belongs to one provider.
A top-level `effort` applies to any provider.

## Reasoning effort

`effort` sets how hard the model reasons on each request. The levels,
by rank: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`,
`ultra`. The Codex adapter sends the level as `reasoning.effort`; the
model catalog says which levels a model takes. The Anthropic adapter
sends `output_config.effort`, next to adaptive thinking; the Messages
API takes `low` through `max`, so the adapter clamps `none` and
`minimal` to `low` and `ultra` to `max`. The level goes on every
request, so a model that does not accept the field refuses the call;
the current models accept it. Unknown keys warn on stderr and do not
stop the run.

`/effort [level]` shows or sets the level for the session. The
full-screen interface names the level on its status.
Each `meta` record in the session file names the effort in force, so
a reader can tell what a run used. A resumed session takes its level
from config, not from the file.

## The approval mode at runtime

`/approval` shows the approval mode. `/approval read-only|ask|auto`
sets it for the turns that follow, in both interfaces. Shift-Tab in
the full-screen interface cycles the mode: `ask`, `auto`,
`read-only`, and back. The completion popup owns Shift-Tab while it
is open. The key does nothing while a turn runs. The full-screen
interface names the mode on its status. The user makes the switch
at the keyboard. The trust rules for a project file do not limit
it: the user can set any mode. A switch swaps the tool set:

- `read-only` drops the write and execute tools.
- `auto` asks nothing.
- `ask` asks again.

A grant from an `always` answer survives a switch.
Each `meta` record in the session file names the mode in force. The
`meta` record spells the mode `read-only`, the command name. An
`approval` record spells it `read_only`, the record's own older
spelling. A reader of the file must accept both. A resumed session
takes its mode from the flags and the config, not from the file. A one-shot run has no command and no key, so its
mode is the one the flags and the config set.

## The model-call limit

`max_model_calls` limits the model calls in one user turn. Each
tool round is one more call, so a long code review uses many calls.
A value is an integer from 1 to the 32-bit maximum. When a turn hits the limit, the
engine stops the turn, and the interface reports the limit and its
value. The session keeps every result up to the stop, and the next
prompt continues from those results. The limit guards against a
runaway tool loop; it is not a spend budget. The project file can
lower the limit. It cannot raise the limit.

## The context on the status

The full-screen interface names the context in use on its status:
`context 34.2k`. The count is the input tokens of the last model
call, the prompt the model just read. Cached tokens count: the
model read them. The totals beside it sum the session's spend
across calls; the two numbers are different measures. A fresh
session clears the count.

When the model's context window is known, the segment adds the
share: `context 34.2k · 17%`. Each provider adapter has a catalog
of known models and their windows. The `context_window` key under
`[providers.<id>]` names the window, in tokens: an integer from 1
to the 32-bit maximum. The key wins over the catalog, so a stale
catalog row has an override. The key applies to every model of its
provider. A model with no window shows the count alone. A window
smaller than the prompt shows a share over 100%. A `/model`
switch moves the window with the model, with no new call.

## Thinking blocks

`tui.thinking` sets how a thinking block starts in the full-screen
interface. A thinking block shows one reasoning summary from the
model. `closed` shows the state and the summary's first line. `open`
shows the whole summary. `hidden` shows no block.

## Themes

`tui.theme` names the colors of the full-screen interface. Three
themes ship:

- `terminal`: the terminal's own palette. This is the default. It
  names palette entries and no RGB value, so it follows the
  terminal's colors.
- `dark`: light text for a dark ground.
- `light`: dark text for a light ground.

A `[theme.<name>]` table sets colors role by role. A table with a
built-in name edits that theme. A table with a new name starts from
`terminal`. Each key is one role:

| Role | What it colors |
|---|---|
| `text` | replies, cards, the input box, the status bar's text |
| `dim` | thinking blocks, notices, a card's output tail and hint, markdown marks, link destinations, the completions not selected, the box's edge and the status on it |
| `accent` | card headers, the selected completion, the browse indicator |
| `error` | failure messages, a failed call's mark |
| `success` | a finished call's mark |
| `warning` | a waiting, declined, or interrupted call's mark; the approval question |
| `heading` | markdown headings |
| `link` | link text |
| `inline_code` | code spans |
| `code_block` | the lines of a code fence |
| `prompt` | the prompt |
| `status_bar` | the status bar's ground, on a screen too small for the box's frame |
| `progress_line` | the progress line |
| `diff_add` | a `+` line of an edit preview |
| `diff_remove` | a `-` line of an edit preview |
| `keyword`, `string`, `comment`, `number` | the highlight classes of a code fence |

A value is `#rrggbb`, one of the sixteen palette names (`black`,
`red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, and
each with a `bright-` prefix), or `none`. `none` takes the color
away; the terminal's own foreground shows, also inside a colored
fence or colored text. A `comment` with no color also dims. A
`status_bar` with no `text` color reverses onto its ground; with
no ground it reverses onto the `text` color. A key
that names no role warns. A value that is no color stops the run. A
name with no built-in theme and no table stops the run.

Only the user file can define tables. A project file can pick a
theme by name, a built-in one or one the user file defines. A
`[theme.<name>]` table in a project file is ignored, with a warning:
a repository must not color the approval question or a preview
away.

The interface fits the theme to the terminal:

- `COLORTERM=truecolor` or `24bit`, or a `TERM` with `direct`:
  every RGB value goes out as it is.
- A `TERM` with `256color`: each RGB value becomes the nearest entry
  of the 256-color palette.
- Any other `TERM`: each RGB value becomes the nearest of the
  sixteen palette entries.
- A non-empty `NO_COLOR`: no color at all. Bold, dim, italic, and
  underline stay, so headings, emphasis, links, marks, and thinking
  still read apart. A non-empty `NO_COLOR` also turns the line
  interface's dim text off.

## Keyboard keys

Mouse capture sends the wheel and the buttons to Demerzel. It is on
by default. `tui.mouse = false` gives the terminal its own text
selection back. Some terminals also select with Shift-drag while
capture is on; that depends on the terminal. Without capture, most
terminals turn the wheel into arrow keys on the alternate screen,
which browse the history.

The full-screen interface has these keys:

- Enter submits the line. Alt-Enter inserts a newline. Shift-Enter
  inserts a newline when the terminal supports the
  keyboard-enhancement flags. A paste keeps its newlines.
- Up and Down move the cursor across the rows of the input box. On
  the first row, Up shows the previous history entry. On the last
  row, Down shows the next entry.
- Home and Ctrl-A move to the start of the line. End and Ctrl-E
  move to the end of the line. While the transcript scrolls, End
  returns it to the bottom.
- Ctrl-U deletes to the start of the line. Ctrl-K deletes to the
  end of the line. Ctrl-W deletes the word before the cursor.
- At the end of the line, Ctrl-K joins the next line. At the start
  of the line, Ctrl-W joins the previous line.
- A line that starts with `/` opens the completion popup. Tab and
  the arrow keys select a command. Enter puts the command on the
  line. The next Enter submits the line. Esc closes the popup. A
  recalled history entry opens no popup; the next edit opens it.
- PageUp and PageDown scroll the transcript. With `tui.mouse` on, the
  mouse wheel scrolls it too.
- With `tui.mouse` on, a drag over the transcript selects text. The
  release copies the text to the clipboard through the terminal, as
  an OSC 52 write. The copy leaves out the marks the frame adds: a
  fence gives its code, a card gives its output and its result line.
  The copy is cut at 64 KiB, and a notice says so. A click clears
  the selection.
- Shift-Tab cycles the approval mode: `ask`, `auto`, `read-only`,
  and back. While the completion popup is open, Shift-Tab selects
  the previous command instead.
- Ctrl-C clears the input box. On an empty input box, Ctrl-C asks
  once. See Quit and resume. During a turn, Ctrl-C stops the turn.
  Ctrl-D on an empty input box ends the session.
- Ctrl-O expands every tool card to its kept output and collapses
  them again.
- Ctrl-T opens the newest thinking block and closes it again.

## Quit and resume

Ctrl-C on an empty input box asks once. The status shows `press
Ctrl-C again to quit` for three seconds. A second press inside that
span ends the session. Any other key takes the question back. A
press on a box with text clears the box and asks nothing. Ctrl-D
and `/exit` end the session at once. The plain interface keeps the
two-press rule and prints the hint on stderr. There, a press on a
line with text discards the line and asks too.

An interactive run that opened a session prints one line on stderr
when it ends. The line comes after the screen restores:

```text
session 01ARZ3NDEKTSV4RRFFQ69G5FAV · resume with demerzel -c
```

A run that opened no session prints nothing. A one-shot run prints
nothing. When a newer session exists in the project, the line names
`--session <id>` instead of `-c`. The command it names always opens
this session.

Two flags resume a session. Both look in the current project only:

- `-c`, `--continue` opens the latest session. With none, it opens
  a fresh one.
- `--session <id>` opens the session with that id. An id with no
  file stops the run with an error.

The two flags exclude each other. `/session` shows the open
session's id and file.

## The debug trace

`DEMERZEL_LOG=<level>` turns the debug trace on for one run. The
levels are `error`, `warn`, `info`, `debug`, and `trace`. The
variable has no config key and no flag: a diagnostic is a variable.
An unset or empty variable turns the trace off. A value that is not
a level stops the run with an error.

The trace writes one file per run:
`$XDG_DATA_HOME/demerzel/logs/<run-start>-<pid>.log`. The run start
is in RFC 3339. The run creates the file at boot. One line on stderr
names the file: `log file: <path>`. In the interfaces the line
follows the ready marker. The file is the one sink in every mode,
because the full-screen interface cannot write to stderr behind the
alternate screen. The file has mode 0600.

Each line holds one event: `<time> <LEVEL> <target> <message>`. The
time is RFC 3339 UTC at millisecond precision. A message never spans
lines. A line at `info` or above reaches the disk as it lands.

Each level adds events:

- `info`: the session create and resume, with the id and the path.
  Each model call, with the provider, the model, the effort, the
  stop reason, the duration, and the usage. Each request attempt,
  with its HTTP status and the endpoint's request id. The stream's
  terminal event, with its reason or its message. Each retry, with
  its delay and its cause. Each tool call, with the name, the
  duration, the output size, the truncation flag, and the tool's
  summary line; for `bash` the summary carries the exit code. Each
  approval decision. Each turn outcome.
- `debug`: the request body size, the SSE event types with their
  counts, and a token refresh.
- `trace`: the request body in full, every SSE payload in full, and
  the conversation itself, message by message.

The trace never holds a credential: no bearer token, no API key, no
auth-store content, at any level. A token refresh logs its status
and not the token.

## Trust rules

A project file travels with the repository, so it cannot raise
privilege, move credentials, or raise cost:

- It cannot raise `approval` above the user file's mode, or above
  `ask`.
- It cannot set `base_url`.
- It cannot turn `history_file` on against a user file that turned
  it off.
- It cannot turn `tui.mouse` on against a user file that turned it
  off. A repository must not take the terminal's own text selection
  from a user who turned capture off to keep it.
- It cannot raise `effort` above the user file's level, or above the
  default `high`.
- It cannot raise `max_model_calls` above the user file's value, or
  above the default `128`.
- It cannot define a theme's colors. It can pick `tui.theme`. A color
  can hide the approval question or a preview.

A flag or a variable decides outright. Ignored project keys warn on
stderr and in `doctor`.

## History

The M2 plan ([`m2-plan.md`](m2-plan.md)) introduced the layered
files and the first keys. M2.5 ([`m2.5-plan.md`](m2.5-plan.md))
added the `[tui]` table. `effort` landed on 2026-08-24, between
M2.5 steps 3 and 4. Cache-hit receipts before that date ran at the
Codex adapter's fixed `low` effort and the Anthropic default.
`tui.mouse` changed its default to `false` on 2026-08-29, after the
M2.5 step-7 dogfood, because capture took the terminal's own text
selection. M2.5 step 8b brought selection and copy into the
interface and set the default back to `true` on the same day.
`tui.theme` and the `[theme.<name>]` tables landed with M2.5 step 8.
`max_model_calls` landed on 2026-08-30, after a code-review
dogfood hit the fixed limit; the default rose from 32 to 128 on
the same day. The context segment and `context_window` landed with
M2.5 step 8e. The two-press quit, the exit line, and `--session`
landed with M2.5 step 8f. `DEMERZEL_LOG` landed on 2026-09-01,
after a dogfood hit a server overload that nothing recorded.
