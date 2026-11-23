// Closes stale issues that never received outside engagement.
//
// Used by .github/workflows/stale-close.yml through actions/github-script, and
// runnable locally as a read-only dry run:
//
//   node .github/scripts/stale-close.cjs [--repo owner/name] [--out report.md] [--min-age-days 30]
//
// Locally the token comes from GITHUB_TOKEN or `gh auth token`. The local
// runner never writes to GitHub.

const MARKER = '<!-- herdr:stale-close -->';
const EXTRA_TEAM = ['akbash-bot', 'kangal-bot'];
const PAGE_SIZE = 25;
const CLOSE_COMMENT = [
  MARKER,
  "We weren't able to reproduce this, and it hasn't had further reports or activity, so it may already be fixed or no longer relevant. Closing as stale.",
  '',
  'If you still see this on the latest release, please open a new issue with current details.',
].join('\n');

const SKIP_REASONS = {
  reactions: 'has reactions',
  outside_comment: 'outside comment',
  linked_pr: 'linked open or merged PR',
  too_many_comments: 'too many comments to evaluate (>100)',
};

const ISSUES_QUERY = `
query($owner: String!, $repo: String!, $cursor: String, $pageSize: Int!) {
  repository(owner: $owner, name: $repo) {
    issues(states: OPEN, first: $pageSize, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id
        number
        title
        url
        createdAt
        author { login __typename }
        labels(first: 20) { nodes { name } }
        reactions { totalCount }
        comments(first: 100) {
          totalCount
          nodes { author { login __typename } }
        }
        closedByPullRequestsReferences(first: 10, includeClosedPrs: false) {
          nodes { number state }
        }
        timelineItems(first: 100, itemTypes: [CROSS_REFERENCED_EVENT, CONNECTED_EVENT]) {
          nodes {
            __typename
            ... on CrossReferencedEvent { source { __typename ... on PullRequest { number state } } }
            ... on ConnectedEvent { subject { __typename ... on PullRequest { number state } } }
          }
        }
      }
    }
  }
  rateLimit { cost remaining }
}`;

const MAINTAINERS_QUERY = `
query($owner: String!, $repo: String!) {
  repository(owner: $owner, name: $repo) {
    object(expression: "HEAD:.github/MAINTAINERS") { ... on Blob { text } }
  }
}`;

const CLOSE_MUTATION = `
mutation($id: ID!, $body: String!) {
  addComment(input: {subjectId: $id, body: $body}) { clientMutationId }
  closeIssue(input: {issueId: $id, stateReason: NOT_PLANNED}) { issue { number state } }
}`;

function parseUserList(content) {
  return content
    .split('\n')
    .map(line => line.trim().toLowerCase())
    .filter(line => line && !line.startsWith('#'));
}

function isTeam(team, author) {
  if (!author) return false;
  const login = author.login.toLowerCase();
  return author.__typename === 'Bot' || login.endsWith('[bot]') || team.has(login);
}

function classify(issue, team) {
  if (issue.reactions.totalCount > 0) return 'reactions';
  if (issue.comments.totalCount > issue.comments.nodes.length) return 'too_many_comments';
  const authorLogin = issue.author?.login.toLowerCase();
  const outside = issue.comments.nodes.some(comment =>
    !isTeam(team, comment.author) &&
    !(comment.author && comment.author.login.toLowerCase() === authorLogin));
  if (outside) return 'outside_comment';
  const prs = [
    ...issue.closedByPullRequestsReferences.nodes,
    ...issue.timelineItems.nodes.map(item => item.source ?? item.subject),
  ].filter(pr => pr && pr.state);
  if (prs.some(pr => pr.state === 'OPEN' || pr.state === 'MERGED')) return 'linked_pr';
  return null;
}

function escapeCell(text) {
  return String(text).replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function renderReport({ repo, dryRun, minAgeDays, candidates, wouldClose, skipped, team }) {
  const verb = dryRun ? 'Would close' : 'Closed';
  const lines = [
    `# Stale issue close (${dryRun ? 'dry run' : 'live'})`,
    '',
    `Repository: \`${repo}\` · minimum age: ${minAgeDays} days · team: ${[...team].sort().join(', ')} + bots`,
    '',
    `- Candidates (open issues older than ${minAgeDays} days): ${candidates}`,
    `- ${verb}: ${wouldClose.length}`,
  ];
  for (const [key, label] of Object.entries(SKIP_REASONS)) {
    lines.push(`- Skipped, ${label}: ${skipped[key]}`);
  }
  lines.push('', `## ${verb}`, '');
  if (wouldClose.length === 0) {
    lines.push('None.');
  } else {
    lines.push('| # | Title | Age (days) | Labels | Author |', '|---|---|---|---|---|');
    for (const issue of wouldClose) {
      lines.push(`| [#${issue.number}](${issue.url}) | ${escapeCell(issue.title)} | ${issue.ageDays} | ${escapeCell(issue.labels.join(', '))} | ${escapeCell(issue.author)} |`);
    }
  }
  return lines.join('\n') + '\n';
}

async function run({ graphql, owner, repo, dryRun, minAgeDays = 30, log = console.log, now = Date.now() }) {
  const maintainers = await graphql(MAINTAINERS_QUERY, { owner, repo });
  const text = maintainers.repository.object?.text;
  if (typeof text !== 'string') throw new Error('Could not read .github/MAINTAINERS');
  const team = new Set([...parseUserList(text), ...EXTRA_TEAM]);

  const cutoff = now - minAgeDays * 24 * 60 * 60 * 1000;
  const wouldClose = [];
  const skipped = Object.fromEntries(Object.keys(SKIP_REASONS).map(key => [key, 0]));
  let candidates = 0;
  let cursor = null;
  let done = false;

  while (!done) {
    const data = await graphql(ISSUES_QUERY, { owner, repo, cursor, pageSize: PAGE_SIZE });
    const { nodes, pageInfo } = data.repository.issues;
    for (const issue of nodes) {
      const created = Date.parse(issue.createdAt);
      if (created >= cutoff) {
        done = true;
        break;
      }
      candidates += 1;
      const reason = classify(issue, team);
      if (reason) {
        skipped[reason] += 1;
        continue;
      }
      wouldClose.push({
        id: issue.id,
        number: issue.number,
        title: issue.title,
        url: issue.url,
        ageDays: Math.floor((now - created) / (24 * 60 * 60 * 1000)),
        labels: issue.labels.nodes.map(label => label.name),
        author: issue.author?.login ?? 'ghost',
      });
    }
    if (!pageInfo.hasNextPage) done = true;
    cursor = pageInfo.endCursor;
  }

  if (!dryRun) {
    for (const issue of wouldClose) {
      await graphql(CLOSE_MUTATION, { id: issue.id, body: CLOSE_COMMENT });
      log(`Closed #${issue.number}`);
    }
  }

  log(`candidates=${candidates} close=${wouldClose.length} skipped=${JSON.stringify(skipped)} dryRun=${dryRun}`);
  const report = renderReport({
    repo: `${owner}/${repo}`, dryRun, minAgeDays, candidates, wouldClose, skipped, team,
  });
  return { candidates, wouldClose, skipped, report };
}

module.exports = { run, classify, parseUserList, CLOSE_COMMENT, MARKER };

async function main() {
  const { execFileSync } = require('node:child_process');
  const fs = require('node:fs');
  const args = process.argv.slice(2);
  const option = (name, fallback) => {
    const index = args.indexOf(name);
    return index === -1 ? fallback : args[index + 1];
  };
  const [owner, repo] = option('--repo', 'herdrdev/herdr').split('/');
  const out = option('--out', null);
  const minAgeDays = Number(option('--min-age-days', '30'));
  const token = process.env.GITHUB_TOKEN ||
    execFileSync('gh', ['auth', 'token'], { encoding: 'utf8' }).trim();

  const graphql = async (query, variables) => {
    if (query.trimStart().startsWith('mutation')) {
      throw new Error('Local runner is read-only');
    }
    const response = await fetch('https://api.github.com/graphql', {
      method: 'POST',
      headers: { Authorization: `bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ query, variables }),
    });
    const body = await response.json();
    if (!response.ok || body.errors) {
      throw new Error(`GraphQL error: ${JSON.stringify(body.errors ?? body)}`);
    }
    return body.data;
  };

  const result = await run({
    graphql, owner, repo, dryRun: true, minAgeDays, log: msg => console.error(msg),
  });
  if (out) fs.writeFileSync(out, result.report);
  else process.stdout.write(result.report);
}

if (require.main === module) {
  main().catch(error => {
    console.error(error);
    process.exit(1);
  });
}
