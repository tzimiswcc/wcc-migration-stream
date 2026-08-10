import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import { requireLoopbackOrigin } from "./loopback";

const require = createRequire(import.meta.url);
const axeSource = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");
const incompleteBaseUrl = requireLoopbackOrigin(
  process.env.WP004_INCOMPLETE_BASE_URL ?? "http://127.0.0.1:4174",
  "WP004_INCOMPLETE_BASE_URL",
);
const governedBaseUrl = requireLoopbackOrigin(
  process.env.WP005_GOVERNED_BASE_URL ?? "http://127.0.0.1:4175",
  "WP005_GOVERNED_BASE_URL",
);
const resultBaseUrl = requireLoopbackOrigin(
  process.env.WP006_RESULT_BASE_URL ?? "http://127.0.0.1:4176",
  "WP006_RESULT_BASE_URL",
);

const TOP_LEVEL_VIEWS = [
  ["Setup / Workspace", "Overview", "/"],
  ["Destination", "Profile", "/profile"],
  ["Source", "Files", "/files"],
  ["Profiling detail", "Profiling", "/profiling"],
  ["Mapping", "Mapping", "/mapping"],
  ["Validate", "Validation", "/validation"],
  ["Issues", "Issues", "/issues"],
  ["Reconciliation", "Reconciliation", "/reconciliation"],
  ["Operations", "Operations", "/operations"],
] as const;

async function expectReadComplete(page: Page) {
  await expect(page.getByText("Loading read-only evidence")).toHaveCount(0);
  await expect(page.getByText("Evidence is unavailable")).toHaveCount(0);
}

test("loads startup context and traverses every top-level read view", async ({
  page,
}) => {
  await page.goto("/");
  await expect(
    page.getByRole("heading", { level: 1, name: "Overview" }),
  ).toBeVisible();
  await expect(page.getByText("Read-only local evidence")).toBeVisible();
  const workspaceIdentity = page.getByLabel("Object workspace identity");
  await expect(workspaceIdentity).toContainText(
    "Synthetic WP-003 governed workspace",
  );
  await expect(workspaceIdentity).toContainText("Workspace ID");
  const journey = page.getByRole("navigation", { name: "Migration journey" });
  await expect(journey).toBeVisible();
  for (const stageId of ["COMPARE", "DECISIONS", "PACKAGE"]) {
    const stage = journey.locator(`[data-stage="${stageId}"]`);
    await expect(stage).toHaveAttribute("data-status", "LOCKED");
    await expect(stage.getByRole("link")).toHaveCount(0);
  }
  await expectReadComplete(page);

  for (const [linkName, heading, path] of TOP_LEVEL_VIEWS) {
    await page
      .getByRole("link", { name: linkName, exact: true })
      .first()
      .click();
    await expect(page).toHaveURL(
      new RegExp(`${path === "/" ? "/$" : `${path}(?:\\?|$)`}`),
    );
    await expect(
      page.getByRole("heading", { level: 1, name: heading }),
    ).toBeVisible();
    await expectReadComplete(page);
    await expect(page.getByText("000001", { exact: true })).toHaveCount(0);
    await expect(page.getByText(/leading zero 0007/)).toHaveCount(0);
  }
});

test("uses authoritative issue filters and opaque pagination", async ({
  page,
}) => {
  await page.goto("/issues?page_size=1");
  await expect(
    page.getByRole("heading", { level: 1, name: "Issues" }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Open exact row" }).first(),
  ).toBeVisible();

  await page.getByLabel("Severity").selectOption("ERROR");
  await page.getByRole("button", { name: "Apply filters" }).click();
  await expect(page).toHaveURL(/severity=ERROR/);
  await expect(page).toHaveURL(/page_size=1/);
  await expectReadComplete(page);

  const issueRegion = page.getByRole("region", { name: "Validation issues" });
  const firstRow = issueRegion.locator("tbody tr");
  await expect(firstRow).toHaveCount(1);
  await expect(firstRow.getByText("ERROR", { exact: true })).toHaveCount(1);
  await expect(firstRow.getByText("WARNING", { exact: true })).toHaveCount(0);
  await expect(firstRow.getByText("INFORMATION", { exact: true })).toHaveCount(
    0,
  );
  const firstRowText = await firstRow.innerText();

  const next = page.getByRole("button", { name: "Next page" });
  await expect(next).toBeEnabled();
  await next.click();
  await expect(page).toHaveURL(/cursor=/);
  await expectReadComplete(page);
  const secondRow = issueRegion.locator("tbody tr");
  await expect(secondRow).toHaveCount(1);
  expect(await secondRow.innerText()).not.toBe(firstRowText);

  await secondRow.getByRole("link", { name: "Open exact row" }).click();
  await expect(
    page.getByRole("heading", { level: 1, name: "Row Detail" }),
  ).toBeVisible();
  await expectReadComplete(page);
  await expect(
    page.getByRole("region", { name: "Immutable received source values" }),
  ).toBeVisible();
});

test("requests exact rows and preserves numeric-looking received text", async ({
  page,
}) => {
  await page.goto("/rows/1?source_file=customer_accounts.csv");
  const received = page.getByRole("region", {
    name: "Immutable received source values",
  });
  await expect(received).toBeVisible();
  await expect(received.getByText("000001", { exact: true })).toBeVisible();

  await page.goto("/rows/3?source_file=customer_accounts.csv");
  const secondReceived = page.getByRole("region", {
    name: "Immutable received source values",
  });
  await expect(secondReceived).toBeVisible();
  await expect(secondReceived).toContainText("leading zero 0007");
  await expect(
    page.getByRole("heading", { name: "Projection and lineage" }),
  ).toBeVisible();
});

test("keeps the actual interrupted workspace visibly non-complete", async ({
  page,
}) => {
  await page.goto(incompleteBaseUrl);
  await expect(
    page.getByRole("heading", { level: 1, name: "Overview" }),
  ).toBeVisible();
  await expect(page.getByText("Read-only local evidence")).toBeVisible();
  await expectReadComplete(page);

  const journey = page.getByRole("navigation", { name: "Migration journey" });
  const sourceStage = journey.locator('[data-stage="SOURCE"]');
  await expect(sourceStage).toHaveAttribute("data-status", "CURRENT");
  await expect(
    sourceStage.getByRole("link", { name: "Source" }),
  ).toHaveAttribute("href", "/files");
  const destinationStage = journey.locator('[data-stage="DESTINATION"]');
  await expect(destinationStage).toHaveAttribute("data-status", "LOCKED");
  await expect(destinationStage.getByRole("link")).toHaveCount(0);

  const jobState = page
    .getByRole("heading", { name: "Job state" })
    .locator("..");
  await expect(
    jobState.getByText("INTERRUPTED", { exact: true }),
  ).toBeVisible();
  await expect(jobState.getByText("COMPLETE", { exact: true })).toHaveCount(0);
});

test("keeps the narrow layout bounded and has no serious axe violations", async ({
  page,
}) => {
  await page.addInitScript({ content: axeSource });
  await page.goto("/");
  await expect(page.getByText("NOT_CHECKED")).toBeVisible();

  const seriousViolations = await page.evaluate(async () => {
    const axe = (
      window as unknown as {
        axe: {
          run(root: Document): Promise<{
            violations: Array<{
              id: string;
              impact: string | null;
              nodes: Array<{ target: string[] }>;
            }>;
          }>;
        };
      }
    ).axe;
    const result = await axe.run(document);
    return result.violations
      .filter(
        (violation) =>
          violation.impact === "serious" || violation.impact === "critical",
      )
      .map(({ id, impact, nodes }) => ({ id, impact, nodes }));
  });
  expect(seriousViolations).toEqual([]);

  await page.setViewportSize({ width: 720, height: 900 });
  await page.goto("/files");
  await expect(
    page.getByRole("region", { name: "Accepted files" }),
  ).toBeVisible();
  const hasHorizontalPageOverflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  );
  expect(hasHorizontalPageOverflow).toBe(false);
});

test("commits one governed decision only after explicit keyboard confirmation", async ({
  page,
}) => {
  await page.addInitScript({ content: axeSource });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`${governedBaseUrl}/decisions`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Decision Queue" }),
  ).toBeVisible();
  await expectReadComplete(page);

  await page
    .getByRole("link", { name: "Open governed detail" })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { level: 1, name: "Issue Decision" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Waive", exact: true }).click();
  await expect(
    page.getByRole("heading", { level: 1, name: "Waive" }),
  ).toBeVisible();
  await page.evaluate(() => {
    document.documentElement.style.zoom = "2";
  });

  const seriousViolations = await page.evaluate(async () => {
    const axe = (
      window as unknown as {
        axe: {
          run(root: Document): Promise<{
            violations: Array<{
              id: string;
              impact: string | null;
              nodes: Array<{ target: string[] }>;
            }>;
          }>;
        };
      }
    ).axe;
    const result = await axe.run(document);
    return result.violations
      .filter(
        (violation) =>
          violation.impact === "serious" || violation.impact === "critical",
      )
      .map(({ id, impact, nodes }) => ({ id, impact, nodes }));
  });
  expect(seriousViolations).toEqual([]);

  await page
    .getByLabel("Practitioner identifier")
    .fill("synthetic-browser-practitioner");
  await page
    .getByLabel("Reason")
    .fill("Bounded synthetic browser waiver evidence.");
  const acknowledgment = page.getByRole("checkbox", {
    name: /I understand Waive keeps the row included/,
  });
  await acknowledgment.focus();
  await page.keyboard.press("Space");
  const review = page.getByRole("button", { name: "Review decision" });
  await review.focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("heading", { name: "Confirm governed decision" }),
  ).toBeVisible();
  const commit = page.getByRole("button", {
    name: "Commit immutable decision",
  });
  await commit.focus();
  await page.keyboard.press("Enter");

  await expect(page.getByText("Durable decision recorded")).toBeVisible();
  await page.getByRole("link", { name: "Inspect history" }).click();
  await expect(
    page.getByRole("heading", { level: 1, name: "Decision History" }),
  ).toBeVisible();
  await expect(
    page.getByText("Bounded synthetic browser waiver evidence."),
  ).toBeVisible();

  const hasHorizontalPageOverflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  );
  expect(hasHorizontalPageOverflow).toBe(false);
});

test("calculates closed WP-006 evidence, drills into exact lineage, and rejects only after confirmation", async ({
  page,
}) => {
  test.skip(
    !process.env.WP006_RESULT_BASE_URL,
    "WP-006 result-approval server was not selected for this bounded proof",
  );
  await page.addInitScript({ content: axeSource });
  await page.goto(`${resultBaseUrl}/result-approval`);
  await expect(
    page.getByRole("heading", {
      level: 1,
      name: "Result Reconciliation & Approval",
    }),
  ).toBeVisible();

  const calculation = page
    .getByRole("heading", { name: "Calculate reconciliation" })
    .locator("..");
  await calculation
    .getByLabel("Practitioner identifier")
    .fill("synthetic-browser-reconciler");
  await calculation
    .getByLabel("Calculation reason")
    .fill("Create bounded synthetic WP-006 reconciliation evidence.");
  await calculation
    .getByRole("checkbox", { name: /calculation records local reconciliation/ })
    .check();
  await calculation
    .getByRole("button", { name: "Calculate immutable reconciliation" })
    .click();
  await expect(
    calculation.getByText("Durable local evidence recorded"),
  ).toBeVisible();
  await calculation
    .getByRole("button", { name: "Return to current evidence" })
    .click();

  const categories = page.getByLabel("Closed row categories");
  await expect(categories.locator(".category-count")).toHaveCount(8);
  const exactLineage = page
    .getByRole("link", { name: "Inspect exact lineage" })
    .first();
  await expect(exactLineage).toBeVisible();
  await exactLineage.click();
  await expect(
    page.getByRole("heading", { name: "Exact row lineage" }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Exact immutable lineage references" }),
  ).toBeVisible();
  await expect(
    page.getByText(/does not expose received or effective values/),
  ).toBeVisible();

  const candidateForm = page
    .getByRole("heading", { name: "Prepare immutable result candidate" })
    .locator("..");
  await candidateForm
    .getByLabel("Candidate creator")
    .fill("synthetic-browser-preparer");
  await candidateForm
    .getByLabel("Preparation reason")
    .fill("Prepare the exact bounded synthetic snapshot for local review.");
  await candidateForm
    .getByRole("checkbox", {
      name: /candidate creation does not grant approval/,
    })
    .check();
  await candidateForm
    .getByRole("button", { name: "Create immutable candidate" })
    .click();
  await expect(
    candidateForm.getByText("Durable local evidence recorded"),
  ).toBeVisible();
  await candidateForm
    .getByRole("button", { name: "Return to current evidence" })
    .click();

  const actionForm = page
    .getByRole("heading", { name: "Record immutable result action" })
    .locator("..");
  await actionForm.getByLabel("Action").selectOption("reject");
  await actionForm
    .getByLabel("Actor / practitioner identifier")
    .fill("synthetic-browser-reviewer");
  await actionForm
    .getByLabel("Mandatory reason")
    .fill("Reject the bounded synthetic candidate after explicit review.");
  const boundary = actionForm.getByRole("checkbox", {
    name: /local Migration Stream readiness evidence only/,
  });
  await boundary.focus();
  await page.keyboard.press("Space");
  const review = actionForm.getByRole("button", { name: "Review rejection" });
  await review.focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("heading", { name: "Confirm local result rejection" }),
  ).toBeVisible();
  const commit = page.getByRole("button", {
    name: "Commit immutable rejection",
  });
  await commit.focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByText("Durable local evidence recorded").last(),
  ).toBeVisible();

  const seriousViolations = await page.evaluate(async () => {
    const axe = (
      window as unknown as {
        axe: {
          run(root: Document): Promise<{
            violations: Array<{
              id: string;
              impact: string | null;
              nodes: Array<{ target: string[] }>;
            }>;
          }>;
        };
      }
    ).axe;
    const result = await axe.run(document);
    return result.violations
      .filter(
        (violation) =>
          violation.impact === "serious" || violation.impact === "critical",
      )
      .map(({ id, impact, nodes }) => ({ id, impact, nodes }));
  });
  expect(seriousViolations).toEqual([]);

  await page.setViewportSize({ width: 720, height: 900 });
  const hasHorizontalPageOverflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  );
  expect(hasHorizontalPageOverflow).toBe(false);
});
