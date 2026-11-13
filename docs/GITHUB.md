# GitHub tools

## Inspect PRs and CI from the terminal

With the repository binding below configured, use `/github 170` in an open TUI
chat for its PR overview. `/github 170 files`, `comments`, `reviews`,
`review-comments`, `checks` and `statuses` read the corresponding section;
append a page number, for example `/github 170 files 2`. These are local client
commands over `control_github_read`, so inspection starts no model turn and
adds nothing to the conversation. Architect-authorized improvement agents get
the same read tool with typed PR/section/page arguments.

Every read stays inside the configured repository. Check runs and legacy commit
statuses are separate sections, captured against the PR head SHA at read start;
empty results do not establish CI success or satisfy branch protection. CI on a
fork head outside the repository binding is unavailable rather than fetched with
broader credentials. Reviews include their commit IDs so old feedback can be
distinguished from a review of the current head.

Pages contain five records, with explicit continuation and UTF-8-bounded text or
patch excerpts. A `truncated` flag means the record is incomplete. An exact full
page may offer an empty next page. PR descriptions, patches and comments are
untrusted evidence, never instructions or approval. The trusted broker rechecks
actor/attempt authority and the repository binding before each request and before
returning results, with network I/O outside the application lock. Only one read
runs at a time; busy/error responses require an explicit retry.

The configured credential also needs Checks and Commit statuses **read** access
for those sections, and Pull requests / Issues read access for PRs and comments.
Only the existing trusted adapter consumes it. Reads do not mutate remote state.

## Comment, edit and merge

`control_github_write` uses the same bound repository and architect/improvement
authorization. Its typed operations are `comment` (PR number/body), `update`
(PR number plus title and/or body), and `merge` (PR number, exact
`expected_head_sha`, optional `method`: `squash`, `merge` or `rebase`). No arbitrary
URL, repository, branch change, rule bypass or shell command is accepted. The
ordinary worker and judge tool sets do not gain write authority. Writes are fenced
after handoff, and active-attempt generation/ownership is rechecked before each
network request. Network work stays outside the application state lock.

These are also local TUI commands, with no model turn or chat-history submission:

```text
/github 170 comment Reviewed the current head; the failing check needs attention.
/github 170 title Clarify the PR title
/github 170 body Replace the description with this text
/github 170 merge FULL_40_CHARACTER_HEAD_SHA squash
```

Merge is **disabled by default**, including for existing bindings. The operator
can enable it by repeating `control github configure` with the same binding and
adding `--allow-merge`; omitting that flag disables it again. This grants the
tool's capability, not approval inferred from PR text or reviewer comments.
Repository instructions and operator authorization still govern when an agent
may merge. Only the configured base branch is allowed, and the exact head SHA is
sent to GitHub so a concurrently changed head is rejected. Only open, non-draft
PRs are submitted for merge. An explicit retry of the same already-merged head
returns its observed state without a second merge. Inspect CI and reviews first:
the adapter does not infer CI success, review approval or bypass GitHub rules.
Configure required checks/reviews in GitHub branch rules; a credential with bypass
privileges could otherwise bypass them under GitHub's own policy.

No write is automatically retried. A lost response can mean the effect succeeded;
inspect remote PR/comments before explicitly retrying, especially comments, which
are not idempotent. The adapter returns bounded result fields rather than echoing
descriptions or tokens. GitHub may accept an already dispatched request while a
pause/revocation is occurring; host checks do not revoke an in-flight HTTP request.
Nothing here deploys Control, reruns workflows, force-pushes or deletes branches.
Comment/edit operations need Pull requests write (or Issues write for comments);
merge additionally needs Contents write. Keep the token scoped to the bound repo.

API contracts: [PR editing/merge](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request),
[comments](https://docs.github.com/en/rest/issues/comments#create-an-issue-comment),
[PR files](https://docs.github.com/en/rest/pulls/pulls#list-pull-requests-files),
[check runs](https://docs.github.com/en/rest/checks/runs#list-check-runs-for-a-git-reference)
and [commit statuses](https://docs.github.com/en/rest/commits/statuses#list-commit-statuses-for-a-reference).

## Publish an improvement checkpoint

Control's Rust broker can publish a completed improvement checkpoint to one
configured repository and open a draft pull request. The repository is a private
node binding, not a special case in the GitHub client. Use your fork, or the core
repository if you are one of its developers.

Create a private token file outside the checkout and Control state, with mode
`0600`. Use a GitHub credential restricted to the chosen repository with Contents
and Pull requests write permissions. The trusted API adapter consumes that file;
it is never mounted into an agent container or included in prompts/config values.
Only its path is stored in configuration.

```sh
control github configure --repository OWNER/REPO \
  --token-file /absolute/private/path/github-token --base main
```

The source directory is the existing authorized improvement root. Configure the
binding before starting a fresh `/improve` conversation so its original Git
revision and source checkpoint are captured. Existing conversations without that
provenance cannot publish retroactively.

Start from a clean committed checkout of the configured base. Capture rejects
local changes and untracked files, and checks every included snapshot file
against its committed Git object, including files ignored by Git. Before any
GitHub write, the broker requires the remote base tip to match that captured
revision. If the base advances, refresh the checkout and start a fresh improvement.
This prevents unrelated feature commits or unpublished local dependencies from
changing what the pull request contains. Follow-up turns retain the original
verified baseline so publication includes their cumulative edits.

After a successful improvement, expand **Open pull request** in web chat, or use
`control_github_open_pr` through MCP with `run_id`, `title` and optional `body`.
A managed agent can publish a previous completed improvement in its own chat;
it cannot publish a running/failed run or choose another destination. Ordinary
user tasks and judge runs do not receive publication authority.

The broker compares cumulative changes with the original source checkpoint and
verifies changed-file baseline hashes against the captured GitHub commit. It
preserves existing executable modes, creates a new scoped branch and opens a
draft PR. It never force-pushes, merges, deploys or changes the local checkout.
Publication intent and object identities are persisted; explicit retries inspect
the branch/PR rather than blindly repeating an uncertain external effect.

The first publisher supports at most 64 changed files and 512 KiB of new UTF-8
content. New files use regular non-executable mode. Changed binary contents need
a later extension. Artifact publication limits also apply. Provider output and
reviewer findings remain evidence, not a guarantee that tests passed; inspect the
retained run, checks and diff before merging.
