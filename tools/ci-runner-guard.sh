#!/usr/bin/env bash
# The desk runner's job-started hook (ACTIONS_RUNNER_HOOK_JOB_STARTED), copied into the runner's directory by
# tools/ci-runner.sh. The runner runs it before every job it picks up, and a job that fails it runs no step.
#
# The workflow's own `if:` can't keep a pull request off the desk: a pull request runs its own copy of the workflow, and a
# fork's can name this runner in any job it adds. This copy lives on the desk, where no workflow reaches before it has run,
# and it lets through only a push to Penna's main.
set -euo pipefail

want_repo=PushMotta/penna
want_ref=refs/heads/main
# The event as GitHub sent it, which the runner writes into its own directory before this runs. It is read from there, not
# from a path a job could name.
payload="$(dirname "$(readlink -f "$0")")/_work/_temp/_github_workflow/event.json"

refuse() {
  echo "The penna runner takes pushes to $want_repo's main only, and refused this job: $*" >&2
  exit 1
}

[ "${GITHUB_REPOSITORY:-}" = "$want_repo" ] || refuse "repository ${GITHUB_REPOSITORY:-unknown}"
[ "${GITHUB_EVENT_NAME:-}" = push ] || refuse "event ${GITHUB_EVENT_NAME:-unknown}"
[ "${GITHUB_REF:-}" = "$want_ref" ] || refuse "ref ${GITHUB_REF:-unknown}"
[ -f "$payload" ] || refuse "no event at $payload"
jq -e --arg ref "$want_ref" --arg repo "$want_repo" \
  '.ref == $ref and .repository.full_name == $repo and (has("pull_request") | not)' "$payload" >/dev/null ||
  refuse "the event is not a push to main"

echo "A push to $want_repo's main, $(jq -r '.after[0:7]' "$payload"): the penna runner takes it."
