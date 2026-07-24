import assert from 'node:assert/strict';

export const captureAttemptTimeoutMs = 20_000;
const captureOperationTimeoutMs = 18_000;
export const maxCaptureAttempts = 4;
export const captureRetryTimeoutMs = captureAttemptTimeoutMs * maxCaptureAttempts;

export async function withTimeout(operation, timeoutMs = captureAttemptTimeoutMs) {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`capture attempt timed out after ${timeoutMs}ms`)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

export function normalizeStyleEntries(entries) {
  return Object.fromEntries(entries.filter(([name]) => !name.startsWith('--')).sort(([a], [b]) => a.localeCompare(b)));
}

export async function retryCapture(operation) {
  let lastError;
  for (let attempt = 1; attempt <= maxCaptureAttempts; attempt++) {
    try {
      return await operation(attempt);
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError;
}

function locator(page, selector) {
  if (selector.type === 'role') return page.getByRole(selector.value, selector.name ? { name: selector.name } : undefined);
  if (selector.type === 'name') return page.locator(`[name=${JSON.stringify(selector.value)}]`);
  if (selector.type === 'text') return page.getByText(selector.value, { exact: true });
  if (selector.type === 'data') return page.locator(`[data-probe=${JSON.stringify(selector.value)}]`);
  return page.locator(`[id=${JSON.stringify(selector.value)}]`);
}

async function waitForCardinality(target, expected, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await target.count() === expected) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`readiness cardinality did not become ${expected}`);
}

async function performAction(page, action) {
  if (!action) return;
  if (action.type === 'press') await page.keyboard.press(action.key);
  else if (action.type === 'hover') await locator(page, action.selector).hover();
  else if (action.type === 'focus') await locator(page, action.selector).focus();
  else await locator(page, action.selector).click();
}

async function capturePage(browser, baseUrl, probe, artifact) {
  let page;
  let finished = false;
  let failure;
  const deadline = Date.now() + captureAttemptTimeoutMs;
  const remaining = () => Math.max(1, deadline - Date.now());
  const consoleMessages = [];
  const pageErrors = [];
  try {
    return await withTimeout(async () => {
      page = await browser.newPage({ viewport: probe.viewport });
      if (finished) {
        void page.close().catch(() => {});
        throw new Error('capture attempt finished before page creation completed');
      }
      page.on('console', (message) => consoleMessages.push(`${message.type()}: ${message.text()}`));
      page.on('pageerror', (error) => pageErrors.push(error.stack ?? error.message));
      await page.goto(new URL(probe.route, baseUrl).href, { waitUntil: 'networkidle', timeout: captureAttemptTimeoutMs });
      await Promise.all([
        page.evaluate(() => document.fonts.ready),
        waitForCardinality(locator(page, probe.readiness.selector), probe.readiness.cardinality, captureAttemptTimeoutMs),
      ]);
      await page.addStyleTag({ content: '*,*::before,*::after{transition:none!important;animation:none!important}' });
      await performAction(page, probe.action);
      await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const elements = await locator(page, probe.selector).evaluateAll((nodes) => nodes.map((node) => {
        const computed = getComputedStyle(node);
        const entries = [];
        for (let index = 0; index < computed.length; index++) {
          const name = computed[index];
          if (!name.startsWith('--')) entries.push([name, computed.getPropertyValue(name)]);
        }
        entries.sort(([a], [b]) => a.localeCompare(b));
        return { identity: node.getAttribute('data-identity'), styles: Object.fromEntries(entries) };
      }));
      if (artifact?.screenshot) await page.screenshot({ path: artifact.screenshot, fullPage: true });
      return { elements, consoleMessages, pageErrors };
    }, captureOperationTimeoutMs);
  } catch (error) {
    failure = error;
    throw error;
  } finally {
    finished = true;
    await withTimeout(() => artifact?.writeDiagnostics?.({
      consoleMessages,
      pageErrors,
      error: failure ? failure.stack ?? failure.message : null,
    }), remaining()).catch(() => {});
    if (failure && artifact?.screenshot && page) {
      await withTimeout(() => page.screenshot({ path: artifact.screenshot, fullPage: true }), remaining()).catch(() => {});
    }
    await withTimeout(() => page?.close(), remaining()).catch(() => {});
  }
}

export async function captureProbe(browser, baseUrl, probe, artifactForAttempt) {
  const capture = await retryCapture((attempt) => capturePage(browser, baseUrl, probe, artifactForAttempt?.(attempt)));
  assert.equal(capture.elements.length, probe.cardinality, 'target cardinality');
  assert.deepEqual(capture.elements.map(({ identity }) => identity), probe.identity, 'target identity sequence');
  return capture;
}

export async function captureAll(browser, baseUrl, probes, artifactFor) {
  return Object.fromEntries(await Promise.all(Object.entries(probes).map(async ([name, probe]) => [
    name,
    await captureProbe(browser, baseUrl, probe, (attempt) => artifactFor?.(name, attempt)),
  ])));
}

export function assertOracle({ baseline, post, withheld, candidateTokens }) {
  assert.deepEqual(
    Object.fromEntries(Object.entries(post).map(([name, value]) => [name, value.elements])),
    Object.fromEntries(Object.entries(baseline).map(([name, value]) => [name, value.elements])),
    'pre/post computed styles, identity, count, and order',
  );
  const changedProperties = [];
  for (const [probeName, capture] of Object.entries(baseline)) {
    capture.elements.forEach((element, index) => {
      const absent = withheld[probeName].elements[index];
      for (const [property, value] of Object.entries(element.styles)) {
        if (absent.styles[property] !== value) changedProperties.push(`${probeName}:${property}`);
      }
    });
  }
  assert.ok(candidateTokens.length > 0, 'causal witness requires expected candidate tokens');
  assert.ok(changedProperties.length > 0, 'authored stylesheet withholding must change a standard computed property');
  return changedProperties;
}
