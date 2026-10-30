# GitHub publication

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
