import assert from "node:assert/strict";

import type {
  Browser,
  CaptureArtifact,
  CaptureSet,
  CapturedElement,
  Locator,
  Page,
  Probe,
  ProbeAction,
  ProbeCapture,
  ProbeSelector,
} from "./types.ts";

export const captureAttemptTimeoutMs = 20_000;
const captureOperationTimeoutMs = 18_000;
export const maxCaptureAttempts = 4;

export async function withTimeout<T>(
  operation: () => T | Promise<T>,
  timeoutMs = captureAttemptTimeoutMs,
): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`capture attempt timed out after ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

export function normalizeStyleEntries(entries: [string, string][]): Record<string, string> {
  return Object.fromEntries(
    entries.filter(([name]) => !name.startsWith("--")).sort(([a], [b]) => a.localeCompare(b)),
  );
}

export async function retryCapture<T>(operation: (attempt: number) => Promise<T>): Promise<T> {
  let lastError: unknown;
  for (let attempt = 1; attempt <= maxCaptureAttempts; attempt++) {
    try {
      return await operation(attempt);
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError;
}

function locator(page: Page, selector: ProbeSelector): Locator {
  if (selector.type === "role") {
    const role = selector.value as Parameters<Page["getByRole"]>[0];
    return page.getByRole(role, selector.name ? { name: selector.name } : undefined);
  }
  if (selector.type === "name") return page.locator(`[name=${JSON.stringify(selector.value)}]`);
  if (selector.type === "text") return page.getByText(selector.value, { exact: true });
  if (selector.type === "data")
    return page.locator(`[data-probe=${JSON.stringify(selector.value)}]`);
  if (selector.type === "tag" || selector.type === "css") return page.locator(selector.value);
  return page.locator(`[id=${JSON.stringify(selector.value)}]`);
}

async function waitForCardinality(
  target: Locator,
  expected: number,
  timeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if ((await target.count()) === expected) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`readiness cardinality did not become ${expected}`);
}

async function performAction(page: Page, action: ProbeAction | undefined): Promise<void> {
  if (!action) return;
  if (action.type === "press") await page.keyboard.press(action.key);
  else if (action.type === "hover") await locator(page, action.selector).hover();
  else if (action.type === "focus") await locator(page, action.selector).focus();
  else await locator(page, action.selector).click();
}

async function capturePage(
  browser: Browser,
  baseUrl: string,
  probe: Probe,
  artifact: CaptureArtifact | undefined,
): Promise<ProbeCapture> {
  let page: Page | undefined;
  let finished = false;
  let failure: unknown;
  const deadline = Date.now() + captureAttemptTimeoutMs;
  const remaining = () => Math.max(1, deadline - Date.now());
  const consoleMessages: string[] = [];
  const pageErrors: string[] = [];
  try {
    return await withTimeout(async () => {
      page = await browser.newPage({ viewport: probe.viewport });
      if (finished) {
        void page.close().catch(() => {});
        throw new Error("capture attempt finished before page creation completed");
      }
      page.on("console", (message) => consoleMessages.push(`${message.type()}: ${message.text()}`));
      page.on("pageerror", (error) => pageErrors.push(error.stack ?? error.message));
      await page.goto(new URL(probe.route, baseUrl).href, {
        waitUntil: "domcontentloaded",
        timeout: captureAttemptTimeoutMs,
      });
      await Promise.all([
        page.evaluate(() => document.fonts.ready),
        waitForCardinality(
          locator(page, probe.readiness.selector),
          probe.readiness.cardinality,
          captureAttemptTimeoutMs,
        ),
      ]);
      await page.addStyleTag({
        content: "*,*::before,*::after{transition:none!important;animation:none!important}",
      });
      await performAction(page, probe.action);
      await page.evaluate(
        () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
      );
      const captured = await locator(page, probe.selector).evaluateAll((nodes) =>
        nodes.map((node) => {
          const computed = getComputedStyle(node);
          const entries: [string, string][] = [];
          for (let index = 0; index < computed.length; index++) {
            const name = computed[index];
            entries.push([name, computed.getPropertyValue(name)]);
          }
          const identity =
            node.getAttribute("data-identity") ||
            node.id ||
            node.getAttribute("aria-label") ||
            node.localName ||
            node.textContent?.trim();
          return { identity, entries };
        }),
      );
      const elements: CapturedElement[] = captured.map(({ identity, entries }) => ({
        identity,
        styles: normalizeStyleEntries(entries),
      }));
      if (artifact?.screenshot)
        await page.screenshot({ path: artifact.screenshot, fullPage: true });
      return { elements, consoleMessages, pageErrors };
    }, captureOperationTimeoutMs);
  } catch (error) {
    failure = error;
    throw error;
  } finally {
    finished = true;
    await withTimeout(
      () =>
        artifact?.writeDiagnostics?.({
          consoleMessages,
          pageErrors,
          error: failure instanceof Error ? (failure.stack ?? failure.message) : (failure ?? null),
        }),
      remaining(),
    ).catch(() => {});
    const failedPage = page;
    if (failure && artifact?.screenshot && failedPage) {
      await withTimeout(
        () => failedPage.screenshot({ path: artifact.screenshot, fullPage: true }),
        remaining(),
      ).catch(() => {});
    }
    await withTimeout(() => page?.close(), remaining()).catch(() => {});
  }
}

export async function captureProbe(
  browser: Browser,
  baseUrl: string,
  probe: Probe,
  artifactForAttempt?: (attempt: number) => CaptureArtifact | undefined,
): Promise<ProbeCapture> {
  const capture = await retryCapture((attempt) =>
    capturePage(browser, baseUrl, probe, artifactForAttempt?.(attempt)),
  );
  assert.equal(capture.elements.length, probe.cardinality, "target cardinality");
  assert.deepEqual(
    capture.elements.map(({ identity }) => identity),
    probe.identity,
    "target identity sequence",
  );
  return capture;
}

export async function captureAll(
  browser: Browser,
  baseUrl: string,
  probes: Record<string, Probe>,
  artifactFor?: (name: string, attempt: number) => CaptureArtifact | undefined,
): Promise<CaptureSet> {
  const captures = await Promise.allSettled(
    Object.entries(probes).map(
      async ([name, probe]): Promise<[string, ProbeCapture]> => [
        name,
        await captureProbe(browser, baseUrl, probe, (attempt) => artifactFor?.(name, attempt)),
      ],
    ),
  );
  const failure = captures.find((capture) => capture.status === "rejected");
  if (failure) throw failure.reason;
  return Object.fromEntries(
    captures.flatMap((capture) => (capture.status === "fulfilled" ? [capture.value] : [])),
  );
}

export interface OracleWitness {
  identity: string | undefined;
  property: string;
  baseline: string;
  withheld: string | undefined;
}

export function assertOracle({
  baseline,
  post,
  withheld,
  candidateTokens,
}: {
  baseline: CaptureSet;
  post: CaptureSet;
  withheld: CaptureSet;
  candidateTokens: string[];
}): Record<string, OracleWitness[]> {
  assert.deepEqual(
    Object.fromEntries(Object.entries(post).map(([name, value]) => [name, value.elements])),
    Object.fromEntries(Object.entries(baseline).map(([name, value]) => [name, value.elements])),
    "pre/post computed styles, identity, count, and order",
  );
  assert.ok(candidateTokens.length > 0, "causal witness requires expected candidate tokens");
  const witnesses: Record<string, OracleWitness[]> = {};
  for (const [probeName, capture] of Object.entries(baseline)) {
    const probeWitnesses: OracleWitness[] = [];
    witnesses[probeName] = probeWitnesses;
    capture.elements.forEach((element, index) => {
      const absent = withheld[probeName]?.elements[index];
      for (const [property, value] of Object.entries(element.styles)) {
        if (!property.startsWith("--") && absent?.styles[property] !== value) {
          probeWitnesses.push({
            identity: element.identity,
            property,
            baseline: value,
            withheld: absent?.styles[property],
          });
        }
      }
    });
    assert.ok(
      probeWitnesses.length > 0,
      `authored stylesheet withholding must change a standard computed property for probe ${JSON.stringify(probeName)}`,
    );
  }
  return witnesses;
}
