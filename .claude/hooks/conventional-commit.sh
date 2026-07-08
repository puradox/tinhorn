#!/usr/bin/env bash
# PreToolUse (Bash) hook: block `git commit` when the subject line does not
# follow Conventional Commits. Reads the tool-call JSON on stdin.
#
# Only enforces when a message is supplied inline with -m/--message (the path
# agents use). Editor-based commits, -F/-C, and --amend --no-edit can't be
# inspected before they run, so they pass through untouched — as do all
# non-commit commands. Allow == exit 0 with no output; deny == print the
# PreToolUse deny JSON.

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"

# Not a git commit (also matches compound commands like `git add … && git commit …`).
if ! printf '%s' "$cmd" | grep -Eq '(^|[;&|]|[[:space:]])git[[:space:]]+commit([[:space:]]|$)'; then
  exit 0
fi

# Pull the first -m/--message value, whether "double", 'single', or bare.
subject="$(printf '%s' "$cmd" | perl -ne '
  if (/(?:-m|--message)(?:=|\s+)(?:"([^"]*)"|'"'"'([^'"'"']*)'"'"'|(\S+))/) {
    my $m = defined($1) ? $1 : defined($2) ? $2 : $3;
    $m =~ s/\n.*//s;          # subject is the first line only
    print $m;
    last;
  }
')"

# No inline message found — nothing to validate here.
[ -z "$subject" ] && exit 0

# type(optional scope)!: description — types per the project convention.
if printf '%s' "$subject" | grep -Eq '^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([a-zA-Z0-9._/ -]+\))?!?: .+'; then
  exit 0
fi

reason="Commit blocked: the subject must follow Conventional Commits — type(optional scope): description.
Got: \"$subject\"
Allowed types: feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert.
Example: feat(cli): add --json output   (append ! before the colon for a breaking change)."

jq -n --arg r "$reason" '{
  hookSpecificOutput: {
    hookEventName: "PreToolUse",
    permissionDecision: "deny",
    permissionDecisionReason: $r
  }
}'
exit 0
