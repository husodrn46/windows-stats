// Runs only from the trusted default branch. Event text and PR filenames are data.
const labels = {
  bug: ['d73a4a', 'A reproducible problem'],
  enhancement: ['a2eeef', 'A feature or improvement'],
  'needs-triage': ['fbca04', 'Awaiting an initial maintainer assessment'],
  'area: sensors': ['5319e7', 'Temperatures and the optional sensor helper'],
  'area: ui': ['1d76db', 'Taskbar widgets, panels and settings'],
  'area: translations': ['0e8a16', 'Interface languages and translations'],
  'area: docs': ['0075ca', 'Documentation and screenshots'],
  'area: automation': ['7057ff', 'Repository workflows and build scripts'],
};

function labelsForFiles(files) {
  const result = new Set();
  for (const file of files) {
    // A rename can affect both the old and new areas.
    for (const path of [file.filename, file.previous_filename].filter(Boolean)) {
      if (/^(helpers\/sensors\/|src\/sensorhost(?:\/|\.rs$)|src\/metrics\/(?:sensors|mod)\.rs$)/.test(path)) result.add('area: sensors');
      if (/^src\/(?:widgets\/|(?:canvas|dwtext|popup|settings|menu|taskbar|taskbar_uia|backdrop)\.rs$)/.test(path)) result.add('area: ui');
      if (/^(locales\/|src\/i18n\.rs$)/.test(path)) result.add('area: translations');
      if (/\.md$/i.test(path) || path.startsWith('docs/')) result.add('area: docs');
      if (/^(\.github\/|scripts\/)/.test(path)) result.add('area: automation');
    }
  }
  return [...result];
}

async function triage({ github, context, core }) {
  const isIssue = context.eventName === 'issues' && context.payload.action === 'opened';
  const isPr = context.eventName === 'pull_request_target'
    && ['opened', 'reopened', 'synchronize'].includes(context.payload.action);
  if (!isIssue && !isPr) return;

  const item = isIssue ? context.payload.issue : context.payload.pull_request;
  if (!item || item.user?.type === 'Bot') return;
  const repo = context.repo;
  // Recheck state and labels: the item can change while the job is waiting.
  const { data: current } = await github.rest.issues.get({ ...repo, issue_number: item.number });
  if (current.state !== 'open') return;

  const wanted = new Set();
  if (isIssue) {
    wanted.add('needs-triage');
    // Also handles the first issue, before a form's label exists in the repo.
    if (/^\[Bug\]:/i.test(current.title)) wanted.add('bug');
    if (/^\[Feature\]:/i.test(current.title)) wanted.add('enhancement');
  } else {
    const files = await github.paginate(github.rest.pulls.listFiles, { ...repo, pull_number: item.number, per_page: 100 });
    for (const label of labelsForFiles(files)) wanted.add(label);
  }

  const existing = new Set(current.labels.map(label => (typeof label === 'string' ? label : label.name).toLowerCase()));
  const missing = [...wanted].filter(label => !existing.has(label.toLowerCase()));
  if (!missing.length) return;
  const repoLabels = await github.paginate(github.rest.issues.listLabelsForRepo, { ...repo, per_page: 100 });
  const available = new Set(repoLabels.map(label => label.name.toLowerCase()));
  for (const name of missing) {
    if (available.has(name.toLowerCase())) continue;
    const [color, description] = labels[name];
    try {
      await github.rest.issues.createLabel({ ...repo, name, color, description });
    } catch (error) {
      if (error.status !== 422) throw error;
      // Another item may have created this label concurrently. Verify it exists;
      // do not hide validation failures or a missing permission.
      await github.rest.issues.getLabel({ ...repo, name });
    }
  }
  // Append only. Maintainers' labels, colors and descriptions are preserved.
  await github.rest.issues.addLabels({ ...repo, issue_number: item.number, labels: missing });
  core.info(`Added ${missing.length} label(s) to #${item.number}.`);
}

module.exports = { triage, labelsForFiles };
