# Default review section

Review the diff for correctness, security, reliability, and maintainability. For each
issue produce a checkbox finding with severity (Critical, High, Medium, Low), a bold
one-line title, `file:line`, and a one-line rationale. Omit empty severity sections.

Use the bash tool to read surrounding code when a hunk lacks the context to judge it.
Prefer a few high-confidence findings over exhaustive nitpicking. End with a one-line
verdict: APPROVE, REQUEST CHANGES, or NEEDS DISCUSSION.
