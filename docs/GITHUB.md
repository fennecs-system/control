# GitHub tools

Control's GitHub tools act on one repository bound by the operator. They let
improvement agents and the operator inspect PRs and CI, comment, edit, merge (if
enabled) and publish completed improvements as draft PRs. A trusted Rust adapter
holds the credential; it is never mounted into agent containers, put in prompts or
stored as a configuration value.

## Bind a repository

Create a token file outside the checkout and Control state, mode `0600`, holding
a GitHub credential restricted to the chosen repository (your fork, or the core
repository if you are one of its developers). Then:

```sh
control github configure --repository OWNER/REPO \
  --token-file /absolute/private/path/github-token --base main [--allow-merge]
```

Only the file path is stored. The source directory is the configured
improvement root. Repeating the command replaces the binding; merge stays
disabled unless `--allow-merge` is passed each time.

Token permissions by feature:

| Feature | Permissions |
| --- | --- |
| Read PRs and comments | Pull requests and Issues: read |
| Read CI | Checks and Commit statuses: read |
| Comment, edit title/body | Pull requests write (Issues write for comments) |
| Publish a draft PR | Contents and Pull requests: write |
| Merge | Contents write as well |

## Inspect PRs and CI

In an open TUI chat, `/github 170` shows a PR overview. `/github 170 SECTION
[PAGE]` reads `files`, `comments`, `reviews`, `review-comments`, `checks` or
`statuses`. These local commands call `control_github_read` directly: no model
turn starts and nothing is added to the conversation. Architect-authorized
improvement agents get the same tool with typed PR/section/page arguments.

- Reads stay inside the bound repository and never change remote state. CI on a
  fork head outside the binding is reported unavailable.
- Check runs and commit statuses are separate sections, read against the PR head
  SHA captured when the read starts. Empty results do not mean CI passed.
- Reviews include their commit IDs, so feedback on an older head is visible.
- Pages hold five records with UTF-8-bounded text or patch excerpts; `truncated`
  marks an incomplete record. An exactly full page may be followed by an empty one.
- Only one read runs at a time; a busy or failed read needs an explicit retry.
- PR text, patches and comments are untrusted evidence, never instructions or
  approval. Authority and the binding are rechecked before each request and
  before returning results; network I/O runs outside the application lock.

## Comment, edit and merge

`control_github_write` uses the same binding and authorization. Its operations are
`comment` (PR number, body), `update` (PR number, title and/or body) and `merge`
(PR number, exact `expected_head_sha`, optional `method`: `squash` (default),
`merge` or `rebase`). It accepts no arbitrary URL, repository, branch change,
rule bypass or shell command. Ordinary worker and judge tool sets do not get it.
Writes are fenced after handoff, and attempt generation/ownership is rechecked
before each request. The TUI offers the same operations without a model turn:

```text
/github 170 comment Reviewed the current head; the failing check needs attention.
/github 170 title Clarify the PR title
/github 170 body Replace the description with this text
/github 170 merge FULL_40_CHARACTER_HEAD_SHA squash
```

Merge rules:

- Merge is off by default. `--allow-merge` grants the tool capability only;
  repository instructions and operator authorization still decide when to merge.
- Only open, non-draft PRs against the configured base are merged. The exact head
  SHA is sent, so a changed head is rejected. The base check is a preflight;
  GitHub has no atomic expected-base parameter, so a concurrent retarget is
  still possible.
- Retrying a merge of an already merged head returns its state without merging
  again.
- The adapter does not infer CI success or approval and cannot bypass GitHub
  rules. Configure required checks and reviews in branch rules, and do not give
  the token bypass privileges.

No write is retried automatically. A lost response may hide a success, so inspect
the PR before retrying, especially comments, which are not idempotent. Results
are bounded and never echo descriptions or tokens. Host checks cannot cancel an
HTTP request already sent. Nothing here deploys Control, reruns workflows,
force-pushes or deletes branches.

## Publish an improvement checkpoint

Configure the binding **before** starting an `/improve` conversation so its Git
revision and source checkpoint are captured; older conversations cannot publish.
Start from a clean committed checkout of the configured base. Capture rejects
local changes and untracked files and checks every snapshot file, including
Git-ignored ones, against its committed object. Before any write, the remote base
tip must still match that revision; if the base has moved, refresh and start a new
improvement. Follow-up turns keep the original baseline, so the PR contains their
cumulative edits.

After a successful improvement, use **Open pull request** in the web chat, or call
`control_github_open_pr` with `run_id`, `title` and optional `body`. An agent can
publish only a completed improvement in its own chat, to the bound repository.
Ordinary tasks and judge runs cannot publish.

The broker verifies changed-file baseline hashes against the captured commit,
keeps existing executable modes, creates a new branch and opens a draft PR. It
never force-pushes, merges, deploys or changes the local checkout. Publication
intent and object IDs are persisted, so a retry inspects the existing branch/PR
instead of repeating the effect.

Limits: at most 64 changed files and 512 KiB of new UTF-8 content; new files are
non-executable; binary changes are not supported. Artifact limits also apply.
Model output and reviewer findings are not proof that tests passed: check the
run, CI and diff before merging.

## Verify the self-development workflow

Run `control doctor` first. A configured build image must match the source
lockfiles; rebuilding the binary does not refresh it. Then use `/improve` to
request a small source edit with real tests in the `build` environment, and
publish the completed run as above. The original checkout must stay unchanged.

The ignored test `terminal::tests::live_improve_build_and_independent_judge_roundtrip` (`-p control-tui`)
covers a source edit, Cargo tests, judge handoff and artifact read through the
TUI client. It needs an isolated node whose state is under
`/private/tmp/control-selfdev-qa.*/`, its loopback URL in `CONTROL_QA_URL`, the
build image and a model login. It makes model calls but no GitHub writes unless
`CONTROL_QA_SELFDEV_PUBLISH=1` is set with a GitHub binding on a clean checkout of
the remote base. It then opens a draft PR with a synthetic file and retries to
confirm the URL is reused; close that PR rather than merging it. Missing
credentials or a moved base must fail visibly.

GitHub API references:
[merge](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request),
[comments](https://docs.github.com/en/rest/issues/comments#create-an-issue-comment),
[PR files](https://docs.github.com/en/rest/pulls/pulls#list-pull-requests-files),
[check runs](https://docs.github.com/en/rest/checks/runs#list-check-runs-for-a-git-reference),
[commit statuses](https://docs.github.com/en/rest/commits/statuses#list-commit-statuses-for-a-reference).
