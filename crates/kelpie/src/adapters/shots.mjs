// Kelpie's shots, run with `node shots.mjs <plan.json>` by the shots adapter.
// The plan names kelpie's tools folder, the dev server's base URL, the hosts
// the page may reach, and each shot. The report goes to the plan's `report`.
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const plan = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const require = createRequire(`${plan.tools}/node_modules/`);
const { chromium } = require('playwright-core');

// Each page gets this long to settle; a slower one is captured as it stands.
const SETTLE_MS = 30_000;
// Past this many, a page's problems are counted rather than listed.
const MAX_PROBLEMS = 20;

const allowed = (host) =>
  plan.hosts.some((h) => (h.startsWith('*.') ? host.endsWith(h.slice(1)) : host === h));

// One shot in its own context: its status, and whether a screenshot was taken.
async function capture(context, shot, note) {
  const page = await context.newPage();
  page.on('pageerror', (e) => note(`error: ${e.message.split('\n')[0]}`));
  // A bare format string such as `%o` carries nothing; the page error beside it does.
  page.on('console', (m) => {
    const text = m.text().split('\n')[0];
    if (m.type() === 'error' && !text.startsWith('Failed to load resource') && !/^%\w$/.test(text)) {
      note(`console: ${text}`);
    }
  });
  page.on('requestfailed', (r) => {
    const why = r.failure()?.errorText ?? 'failed';
    if (!why.includes('BLOCKED_BY_CLIENT')) note(`failed ${r.url()}: ${why}`);
  });
  page.on('response', (r) => {
    const main = r.request().isNavigationRequest() && r.frame() === page.mainFrame();
    if (r.status() >= 400 && !main) {
      note(`HTTP ${r.status()} ${r.url()}`);
    }
  });
  let status = null;
  try {
    const response = await page.goto(`${plan.base}${shot.route}`, {
      waitUntil: 'networkidle',
      timeout: SETTLE_MS,
    });
    status = response?.status() ?? null;
  } catch (e) {
    note(`did not settle in ${SETTLE_MS / 1000}s: ${e.message.split('\n')[0]}`);
  }
  try {
    await page.screenshot({ path: shot.file });
    return { status, taken: true };
  } catch (e) {
    note(`no screenshot: ${e.message.split('\n')[0]}`);
    return { status, taken: false };
  }
}

// Every host but the dev server's and the preview's fails to resolve, an IP
// literal included, so a WebSocket the route below never sees fails too.
const browser = await chromium.launch({
  headless: true,
  args: [`--host-resolver-rules=${plan.resolverRules}`],
});
const report = [];
for (const shot of plan.shots) {
  const problems = [];
  const note = (p) => problems.push(p);
  const context = await browser.newContext({
    viewport: { width: shot.width, height: shot.height },
    deviceScaleFactor: shot.mobile ? 2 : 1,
    isMobile: shot.mobile,
    hasTouch: shot.mobile,
    colorScheme: shot.scheme,
  });
  await context.route('**', (route) => {
    const url = new URL(route.request().url());
    if (!url.protocol.startsWith('http') || allowed(url.hostname)) return route.continue();
    note(`blocked ${url.href}: ${url.hostname} is not a preview domain`);
    return route.abort('blockedbyclient');
  });
  let taken = { status: null, taken: false };
  try {
    taken = await capture(context, shot, note);
  } finally {
    await context.close();
  }
  const listed = [...new Set(problems)];
  const more = listed.length - MAX_PROBLEMS;
  report.push({
    ...taken,
    problems: more > 0 ? [...listed.slice(0, MAX_PROBLEMS), `and ${more} more`] : listed,
  });
}
await browser.close();

// A page that ignores prefers-color-scheme gives the same dark shot as light.
plan.shots.forEach((shot, i) => {
  const light = plan.shots.findIndex(
    (s) => s.route === shot.route && s.width === shot.width && s.scheme === 'light',
  );
  if (shot.scheme !== 'dark' || light < 0 || !report[i].taken || !report[light].taken) return;
  if (readFileSync(shot.file).equals(readFileSync(plan.shots[light].file))) {
    report[i].problems.push('identical to the light shot: the page ignores prefers-color-scheme');
  }
});
writeFileSync(plan.report, JSON.stringify(report, null, 2));
