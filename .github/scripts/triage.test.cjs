const { test } = require('node:test');
const assert = require('node:assert/strict');
const { triage, labelsForFiles } = require('./triage.cjs');

function fixture({ event = 'issues', action = 'opened', title = '[Bug]: test', state = 'open', type = 'User', labels = [], repoLabels = [], files = [], createError, lookupError } = {}) {
  const calls = [];
  const item = { number: 6, user: { type } };
  const context = {
    eventName: event,
    repo: { owner: 'husodrn46', repo: 'windows-stats' },
    payload: { action, [event === 'issues' ? 'issue' : 'pull_request']: item },
  };
  const record = (method, result) => async args => { calls.push({ method, ...args }); return result; };
  const github = {
    rest: {
      issues: {
        get: record('get', { data: { state, title, labels } }),
        listLabelsForRepo: 'listLabelsForRepo',
        createLabel: async args => {
          calls.push({ method: 'createLabel', ...args });
          if (createError) throw createError;
        },
        getLabel: async args => {
          calls.push({ method: 'getLabel', ...args });
          if (lookupError) throw lookupError;
          return { data: { name: args.name } };
        },
        addLabels: record('addLabels'),
      },
      pulls: { listFiles: 'listFiles' },
    },
    paginate: async (method, args) => {
      calls.push({ method, ...args });
      return method === 'listFiles' ? files : repoLabels;
    },
  };
  return { calls, run: () => triage({ github, context, core: { info() {} } }) };
}

test('a first bug report creates missing labels before applying them', async () => {
  const f = fixture();
  await f.run();
  assert.deepEqual(f.calls.filter(c => c.method === 'createLabel').map(c => c.name), ['needs-triage', 'bug']);
  assert.deepEqual(f.calls.at(-1), { method: 'addLabels', owner: 'husodrn46', repo: 'windows-stats', issue_number: 6, labels: ['needs-triage', 'bug'] });
});

test('feature form uses enhancement and leaves existing label definitions alone', async () => {
  const f = fixture({ title: '[Feature]: font size', labels: [{ name: 'enhancement' }], repoLabels: [{ name: 'needs-triage' }, { name: 'enhancement' }] });
  await f.run();
  assert.equal(f.calls.some(c => c.method === 'createLabel'), false);
  assert.deepEqual(f.calls.at(-1).labels, ['needs-triage']);
});

test('a free-form issue only receives needs-triage', async () => {
  const f = fixture({ title: 'Help with my graphics card' });
  await f.run();
  assert.deepEqual(f.calls.at(-1).labels, ['needs-triage']);
});

test('the changed files in PR #6 select sensors and UI without guessing a change type', async () => {
  const f = fixture({ event: 'pull_request_target', files: [{ filename: 'src/metrics/mod.rs' }, { filename: 'src/widgets/mod.rs' }] });
  await f.run();
  assert.deepEqual(f.calls.at(-1).labels, ['area: sensors', 'area: ui']);
  assert.equal(f.calls.find(c => c.method === 'listFiles').per_page, 100);
});

test('synchronize adds new areas while preserving manually assigned labels', async () => {
  const f = fixture({ event: 'pull_request_target', action: 'synchronize', labels: [{ name: 'maintainer-reviewed' }, { name: 'AREA: SENSORS' }], files: [{ filename: 'helpers/sensors/Program.cs' }, { filename: 'locales/tr.toml' }] });
  await f.run();
  assert.deepEqual(f.calls.at(-1).labels, ['area: translations']);
});

test('reopened PRs are classified, and an already labeled item makes no writes', async () => {
  const f = fixture({ event: 'pull_request_target', action: 'reopened', labels: ['area: docs'], files: [{ filename: 'README.md' }] });
  await f.run();
  assert.deepEqual(f.calls.map(c => c.method), ['get', 'listFiles']);
});

test('all pages are inspected using paginate, and old and new rename paths count', () => {
  const files = Array.from({ length: 100 }, () => ({ filename: 'Cargo.toml' }));
  files.push({ filename: 'docs/old-sensors.md', previous_filename: 'helpers/sensors/notes.txt' });
  assert.deepEqual(labelsForFiles(files), ['area: docs', 'area: sensors']);
  assert.deepEqual(labelsForFiles([{ filename: '.github/workflows/triage.yml' }, { filename: 'scripts/build.ps1' }, { filename: 'src/i18n.rs' }]), ['area: automation', 'area: translations']);
});

test('bot items, untrusted PR events and label events cannot cause a write loop', async () => {
  for (const options of [{ type: 'Bot' }, { event: 'pull_request' }, { action: 'labeled' }, { action: 'edited' }]) {
    const f = fixture(options);
    await f.run();
    assert.deepEqual(f.calls, []);
  }
});

test('an item closed while queued is ignored', async () => {
  const f = fixture({ state: 'closed' });
  await f.run();
  assert.deepEqual(f.calls.map(c => c.method), ['get']);
});

test('a concurrent label creation is verified before use', async () => {
  const f = fixture({ createError: { status: 422 } });
  await f.run();
  assert.equal(f.calls.filter(c => c.method === 'getLabel').length, 2);
  assert.equal(f.calls.at(-1).method, 'addLabels');
});

test('API permission and validation errors fail instead of silently claiming success', async () => {
  for (const status of [403, 500]) {
    const f = fixture({ createError: { status } });
    await assert.rejects(f.run(), error => error.status === status);
    assert.equal(f.calls.some(c => c.method === 'addLabels'), false);
  }
  const f = fixture({ createError: { status: 422 }, lookupError: { status: 404 } });
  await assert.rejects(f.run(), error => error.status === 404);
  assert.equal(f.calls.some(c => c.method === 'addLabels'), false);
});

test('issue text and filenames are handled as data', async () => {
  const f = fixture({ title: '[Bug]: $(touch /tmp/should-not-exist) ${{ secrets.TOKEN }}' });
  await f.run();
  assert.deepEqual(f.calls.at(-1).labels, ['needs-triage', 'bug']);
  assert.deepEqual(labelsForFiles([{ filename: '$(touch /tmp/should-not-exist)' }]), []);
});
