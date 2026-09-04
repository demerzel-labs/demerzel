#!/usr/bin/env bash
# Drive the full-screen interface under tmux and save screen captures.
#
# The session runs the release binary against the configured provider
# with the ask approval mode, so the captures show the real screen:
# the cards, the progress line, the question, and the status on the
# input box's edge.
# Each capture is the 120x40 pane as text, written to the captures
# directory (default: target/dogfood). Read them, or diff two runs.
#
# Usage: bench/dogfood/run.sh [captures-dir]
# Needs: tmux, target/release/demerzel, a logged-in provider.
set -u
cd "$(dirname "$0")/../.."

OUT=${1:-target/dogfood}
SESSION=dmz-dogfood
mkdir -p "$OUT"
tmux kill-session -t "$SESSION" 2>/dev/null
tmux new-session -d -s "$SESSION" -x 120 -y 40 'target/release/demerzel'

cap() { tmux capture-pane -pt "$SESSION"; }
send() { tmux send-keys -t "$SESSION" "$@"; }

# wait_for <name> <needle> [timeout-s]: poll the screen until the
# needle shows, then save the capture. The needle must belong to the
# state under test alone: "approve bash? yes" from an earlier turn
# also contains "approve bash?", so the question waits on "[y]es".
wait_for() {
  local name=$1 needle=$2 limit=${3:-90} i=0
  while ! cap | grep -qF -- "$needle"; do
    sleep 0.2
    i=$((i + 1))
    if [ "$i" -ge $((limit * 5)) ]; then
      echo "timeout waiting for: $needle"
      cap > "$OUT/$name.txt"
      return 1
    fi
  done
  cap > "$OUT/$name.txt"
  echo "captured $name"
}

snap() { cap > "$OUT/$1.txt"; echo "captured $1"; }

wait_for 00-prompt '❯' 20

# A bash call with a question: the pending card, the spinner while
# it runs, the done card with its tail and duration.
send 'run sleep 4 && echo dogfood-ok' Enter
wait_for 01-question '[y]es'
sleep 3
send y
sleep 1.5
snap 02-running
wait_for 03-done 'dogfood-ok (exit' 120
sleep 8
snap 04-idle

# A thinking block. The endpoint sends a summary only for harder
# work; a one-line bash call draws none, so this asks for a design
# comparison. The needle names the block row, not the progress line
# (which ends after "thinking"). The live row, the closed row with
# its duration, then Ctrl-T opens the newest block and closes it.
send 'read docs/m2.5-plan.md, then compare the row-count browse anchor with a block anchor and recommend one in three sentences' Enter
wait_for 04a-thinking '· thinking ·' 60
wait_for 04b-thought 'thought for' 180
sleep 8
send C-t
sleep 0.5
snap 04c-thought-open
send C-t
sleep 0.5
snap 04d-thought-closed

# A tall output: the five-line tail, the expand key, browse mode.
send 'run seq 1 40' Enter
wait_for 05-question '[y]es'
send y
wait_for 06-tail 'more lines · Ctrl-O' 120
sleep 6
send C-o
sleep 0.5
snap 07-expanded
send PPage
sleep 0.5
snap 08-pageup
send End
send C-o
sleep 0.5
snap 09-collapsed

# A declined call.
send 'run ls /' Enter
wait_for 10-question '[y]es'
send n
wait_for 11-declined 'declined' 120
sleep 6
snap 12-idle

# A long structured reply: an ASCII diagram of the codebase inside
# a code fence. The captures show whether the fence, the frame, and
# the wrap keep the diagram readable at 120 columns, in follow mode
# and browsed. The model ends the reply with a marker, the only
# needle a text reply offers; the prompt spells the marker apart, so
# the echoed prompt cannot match it. Then copy mode selects three rows and
# pastes them into the input box with bracketed paste, as a terminal
# would: the rendered rows must come back as text. The fence frame
# comes with them; the transcript copy leaves it out.
send 'describe this codebase in ascii visuals of the flow and the core components, inside one code fence; end your reply with one line that holds the word DONE, a space, then the word DIAGRAM' Enter
wait_for 13-diagram-live '╭─' 180
wait_for 14-diagram-done 'DONE DIAGRAM' 300
sleep 6
snap 15-diagram-tail
send PPage
send PPage
sleep 0.5
snap 16-diagram-browse
send End
sleep 0.5
tmux copy-mode -t "$SESSION"
tmux send-keys -t "$SESSION" -X top-line
tmux send-keys -t "$SESSION" -X begin-selection
tmux send-keys -t "$SESSION" -X cursor-down
tmux send-keys -t "$SESSION" -X cursor-down
tmux send-keys -t "$SESSION" -X end-of-line
tmux send-keys -t "$SESSION" -X copy-selection-and-cancel
tmux paste-buffer -p -t "$SESSION"
sleep 0.5
snap 17-diagram-pasted
send C-c

send '/exit' Enter
sleep 1
tmux kill-session -t "$SESSION" 2>/dev/null
echo "captures: $OUT"
