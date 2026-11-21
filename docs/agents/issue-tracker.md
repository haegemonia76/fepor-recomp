# Issue tracker: Gitea

Issues and specs for this repo live as Gitea issues. Use the [`tea`](https://gitea.com/gitea/tea) CLI for all operations.

## Conventions

- **Create an issue**: `tea issues create --title "..." --description "..."`. Use a heredoc for multi-line descriptions.
- **Read an issue**: `tea issues <index> --comments`. Use `-o json` for machine-readable output.
- **List issues**: `tea issues list -o json` with appropriate `--labels` and `--state` filters. `--kind pulls` returns PRs instead, `--kind all` returns both.
- **Comment on an issue**: `tea comments add <index> "..."` (the historical shorthand `tea comment <index> "..."` also works).
- **Apply / remove labels**: `tea issues edit <index> --add-labels "..."` / `--remove-labels "..."`.
- **Close**: `tea issues close <index>`. `tea issues close` does not accept a closing comment, so post the explanation first with `tea comments add <index> "..."`, then close.

Infer the repo from `git remote -v`; `tea` does this automatically when run inside a clone. Use `--repo <owner>/<name>` or `--remote <name>` to override.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `tea pulls` equivalents:

- **Read a PR**: `tea pulls <index> --comments`.
- **List external PRs for triage**: `tea pulls list -o json`, then keep only PRs whose author is not a project member/owner (a contributor's PR, not a maintainer's in-flight work; cross-check with `tea api '/repos/{owner}/{repo}/collaborators/<author>'`, which returns 404 for non-collaborators).
- **Comment / label / close**: `tea comments add`, `tea pulls edit --add-labels`/`--remove-labels`, `tea pulls close`.

Gitea numbers issues and PRs from the same sequence per repo (like GitHub), so a bare `#42` may be either: resolve with `tea pulls 42` and fall back to `tea issues 42`.

## When a skill says "publish to the issue tracker"

Create a Gitea issue.

## When a skill says "fetch the relevant ticket"

Run `tea issues <index> --comments`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `tea issues create --labels wayfinder:map`.
- **Child ticket**: an issue carrying `Part of #<map>` at the top of its description and labels `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev (`tea issues edit <n> --add-assignees @me`).
- **Blocking**: Gitea's **native issue dependencies** (requires "Enable dependencies for issues and pull requests" on the repo unit settings; on by default). Add an edge with `tea api -X POST '/repos/{owner}/{repo}/issues/<child>/dependencies' -F index=<blocker-index>`, where `<blocker-index>` is the blocker's `#number` (unlike GitHub, Gitea's dependency API takes the issue index directly, not a numeric database id). Where dependencies are disabled on the repo, fall back to a `Blocked by: #<n>, #<n>` line at the top of the description. A ticket is unblocked when every blocker is closed.
- **Frontier query**: `tea issues list -o json` scoped to the map's children, drop any with an open blocker (`tea api '/repos/{owner}/{repo}/issues/<n>/dependencies'` returns an open issue, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `tea issues edit <n> --add-assignees @me`, the session's first write.
- **Resolve**: `tea comments add <n> "<answer>"`, then `tea issues close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.

## Repository visibility when creating repos

**Private by default: yes.** _(Set to `no` if this workflow intentionally creates public repos.)_

`tea` has no config-level default for repository visibility, and no server-side setting fixes this either:

- `tea repos create`'s `--private` flag is never read from `~/.config/tea/config.yml`. The config's `preferences.flag_defaults` block (`FlagDefaults` in tea's `modules/config/config.go`) supports exactly one override, `remote`; there is no `private` field to default there.
- Gitea's server-side `DEFAULT_PRIVATE` setting (`[repository]` in `app.ini`) only changes the default checkbox state in the web UI's "New repository" form. It has no effect on `tea repos create`, because tea's request always sends an explicit `private: true`/`false` in the API body (the SDK's `CreateRepoOption.Private` field has no `omitempty`), so there is never a missing value for the server to fall back on.

Practical effect: when set to `yes`, any skill, agent, or script that runs `tea repos create` in this repo's workflows (e.g. scaffolding a companion repo, forking for a PR) **must pass `--private` explicitly on every invocation** — there is no global switch to flip instead. When set to `no`, omit `--private` (or pass it only when a specific repo should be private) and don't add it automatically.
