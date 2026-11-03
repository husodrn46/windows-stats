# Repository labels and contribution flow

The existing bug and feature forms accept English or Turkish. A pull request template asks contributors to explain the behavior and record the checks they actually ran, including any Windows or hardware limitations.

## Label rules

`.github/workflows/triage.yml` runs a short Ubuntu job only when a human opens an issue or opens, reopens or updates a pull request. It ignores bot authors and bot-triggered events. It does not run on comments, edits or label changes, so adding a label cannot trigger it again.

| Item | Automatic labels |
|---|---|
| New issue | `needs-triage` |
| Bug form / title beginning `[Bug]:` | `bug` |
| Feature form / title beginning `[Feature]:` | `enhancement` |
| Sensor helper, sensor protocol or sensor metrics PR | `area: sensors` |
| Taskbar widgets, panels, settings or drawing PR | `area: ui` |
| Locale files or translation loader PR | `area: translations` |
| Markdown or documentation images PR | `area: docs` |
| GitHub configuration or scripts PR | `area: automation` |

The exact paths and label definitions live in `.github/scripts/triage.cjs`. All changed-file pages and both sides of a rename are considered. Multiple areas can apply. Area labels are additive when a PR changes: they record affected areas and do not remove labels a maintainer may have set. Maintainers can adjust labels and remove `needs-triage` after assessing an issue.

Missing required labels are created before being applied. Existing labels, colors and descriptions are preserved; concurrent creation is verified with a lookup. The forms use GitHub's native `labels` field. If those labels do not exist yet, title-prefix detection lets the first workflow run create and apply them. Unknown issue types receive only `needs-triage`; no priority, approval or release promise is inferred.

## Permissions and cost

The workflow needs repository contents read access for its own script, pull requests read access for filenames, and issues write access for the label API. It uses only `GITHUB_TOKEN`. Actions are pinned to commit hashes, checkout credentials are not persisted, and the job times out after two minutes. There is no schedule or build step. Standard GitHub-hosted runners are free for public repositories such as this one; if the repository ever became private, the job would count against the account's Actions minutes.

For fork PRs, `pull_request_target` uses the trusted default branch. Checkout explicitly selects this repository's default branch and only the automation directory; PR code is never checked out, imported or executed. Titles and filenames are handled as data. Keep that boundary when editing the workflow. GitHub Actions or organization event policies may restrict `pull_request_target`; check those policies if no label job runs. See [GitHub's security guidance](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target) and [label API permissions](https://docs.github.com/en/rest/issues/labels).

The workflow takes effect after reaching the default branch and only handles future matching events; it does not backfill old issues or PRs. No workflow run is needed to validate the label decisions locally:

```sh
node --test .github/scripts/triage.test.cjs
```

The test client is fake. It covers new issues, fork-style PR events, missing labels, retries, pagination usage, rename handling, existing labels, bot/closed items, errors and untrusted strings. It does not prove the live token's permissions or repository policy. The Windows build excludes changes limited to these scripts or the label workflow to avoid spending Windows runner time on label-only changes; run the Node tests before submitting them.
