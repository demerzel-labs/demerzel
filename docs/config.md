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
history_file = true              # file-backed input history

[providers.openai-codex]
base_url = "https://chatgpt.com/backend-api/codex"
model = "gpt-5.6-sol"
effort = "medium"                # per-provider override

[providers.anthropic]
base_url = "https://api.anthropic.com"
model = "claude-sonnet-5"

[tui]
mouse = true                     # wheel scroll in the full screen
markdown = true                  # render model replies as markdown
thinking = "closed"              # closed | open | hidden
```

| Key | Flag | Variable | Default |
|---|---|---|---|
| `provider` | `--provider` | `DEMERZEL_PROVIDER` | `openai-codex` |
| `model` | `--model` | `DEMERZEL_MODEL` | the provider's default |
| `base_url` | `--base-url` | `DEMERZEL_BASE_URL` | the provider's endpoint |
| `approval` | `--auto`, `--read-only` | — | `ask`; one-shot runs read-only |
| `effort` | `--effort` | `DEMERZEL_EFFORT` | `high` |
| `history_file` | — | — | `true` |
| `tui.mouse` | — | — | `true` |
| `tui.markdown` | — | — | `true` |
| `tui.thinking` | — | — | `closed` |

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

`/effort [level]` shows or sets the level for the session.
Each `meta` record in the session file names the effort in force, so
a reader can tell what a run used. A resumed session takes its level
from config, not from the file.

## Thinking blocks

`tui.thinking` sets how a thinking block starts in the full-screen
interface. A thinking block shows one reasoning summary from the
model. `closed` shows the state and the summary's first line. `open`
shows the whole summary. `hidden` shows no block.

## Keyboard keys

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
  line. The next Enter submits the line. Esc closes the popup.
- PageUp and PageDown scroll the transcript. The mouse wheel scrolls
  it too.
- Ctrl-C clears the input box. During a turn, Ctrl-C stops the
  turn. Ctrl-D on an empty input box ends the session.
- Ctrl-O expands every tool card to its kept output and collapses
  them again.
- Ctrl-T opens the newest thinking block and closes it again.

## Trust rules

A project file travels with the repository, so it cannot raise
privilege, move credentials, or raise cost:

- It cannot raise `approval` above the user file's mode, or above
  `ask`.
- It cannot set `base_url`.
- It cannot turn `history_file` or `tui.mouse` on against a user file
  that turned them off.
- It cannot raise `effort` above the user file's level, or above the
  default `high`.

A flag or a variable decides outright. Ignored project keys warn on
stderr and in `doctor`.

## History

The M2 plan ([`m2-plan.md`](m2-plan.md)) introduced the layered
files and the first keys. M2.5 ([`m2.5-plan.md`](m2.5-plan.md))
added the `[tui]` table. `effort` landed on 2026-08-24, between
M2.5 steps 3 and 4. Cache-hit receipts before that date ran at the
Codex adapter's fixed `low` effort and the Anthropic default.
