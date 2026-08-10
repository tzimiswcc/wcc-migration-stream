import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import { requireLoopbackOrigin } from "./loopback";

const selectedOrigin = process.env.FRESH_ENGAGEMENT_BASE_URL;
const freshEngagementOrigin = requireLoopbackOrigin(
  selectedOrigin ?? "http://127.0.0.1:4177",
  "FRESH_ENGAGEMENT_BASE_URL",
);
const sourceZip = fileURLToPath(
  new URL("../../fixtures/packages/valid.zip", import.meta.url),
);

test.skip(
  !selectedOrigin,
  "A fresh engagement server was not selected for this bounded proof",
);

test("proves the complete fresh-project-to-first-issues presenter path", async ({
  page,
}) => {
  const browserErrors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") browserErrors.push(message.text());
  });
  page.on("pageerror", (error) => browserErrors.push(error.message));

  await page.goto(`${freshEngagementOrigin}/setup`);
  await expect(
    page.getByRole("heading", { name: "Local migration readiness proof" }),
  ).toBeVisible();
  await expect(
    page.getByRole("navigation", { name: "Migration journey" }),
  ).toContainText("Issues");
  await expect(
    page
      .getByRole("navigation", { name: "Migration journey" })
      .getByRole("link"),
  ).toHaveCount(1);
  await expect(
    page.getByRole("link", { name: "Setup / Workspace", exact: true }),
  ).toHaveAttribute("href", "/setup");

  await expect(
    page.getByRole("heading", { name: "Initialize this object workspace" }),
  ).toBeVisible();
  await page
    .getByLabel("Workspace display name")
    .fill("WP-006 browser workspace");
  await page
    .getByLabel("Migration object name or scope")
    .fill("Synthetic customer readiness");
  await page
    .getByLabel("Creator / operator attribution")
    .fill("WP-006 browser proof");
  await page
    .getByRole("button", { name: "Initialize object workspace" })
    .click();
  const workspaceIdentity = page.getByLabel("Object workspace identity");
  await expect(workspaceIdentity).toContainText("WP-006 browser workspace");
  await expect(workspaceIdentity).toContainText("Workspace ID");

  await page
    .getByLabel("Engagement name")
    .fill("Tour 1 presenter readiness proof");
  await page.getByRole("button", { name: "Create local engagement" }).click();

  await page.getByLabel("Source ZIP").setInputFiles(sourceZip);
  await page.getByRole("button", { name: "Run local safety check" }).click();
  await expect(
    page.getByText("Package passed the local safety check."),
  ).toBeVisible();
  await expect(page.getByText("customer_accounts.csv")).toBeVisible();
  await page
    .getByRole("checkbox", { name: /Accept this exact manifest/ })
    .check();
  await page.getByRole("button", { name: "Accept source locally" }).click();

  await expect(
    page.getByRole("heading", { name: "Source profiled locally" }),
  ).toBeVisible();
  await expect(
    page.getByText("Value samples are not stored for this engagement."),
  ).toBeVisible();
  await expect(page.getByText("000001", { exact: true })).toHaveCount(0);

  await page
    .getByRole("heading", {
      name: "Synthetic customer readiness proof pack",
    })
    .locator("..")
    .getByRole("button", {
      name: "Use this proof pack: Synthetic customer readiness proof pack",
    })
    .click();
  await expect(
    page.getByRole("heading", {
      name: "Review the canonical proof profile",
    }),
  ).toBeVisible();
  await expect(
    page.getByText(/Registration preserves REVIEWED_DRAFT/),
  ).toBeVisible();

  const approval = page
    .getByRole("heading", { name: "Approve for this local proof" })
    .locator("..");
  await approval.getByLabel("Approver").fill("Tour 1 presenter");
  await approval
    .getByLabel("Approval reason")
    .fill("Reviewed the exact controlled Tour 1 proof profile and rules.");
  for (const acknowledgment of await approval.getByRole("checkbox").all()) {
    await acknowledgment.check();
  }
  await approval
    .getByRole("button", {
      name: "Approve for local mapping and validation",
    })
    .click();

  await expect(
    page.getByRole("heading", {
      name: "Review controlled mapping suggestions",
    }),
  ).toBeVisible();
  await page.getByLabel("Mapping author").fill("Tour 1 presenter");
  await page
    .getByRole("checkbox", {
      name: /I reviewed the exact controlled suggestions/,
    })
    .check();
  await page.getByRole("button", { name: "Check and prepare mapping" }).click();

  await expect(
    page.getByRole("heading", { name: "Mapping coverage" }),
  ).toBeVisible();
  await expect(
    page.getByText(
      "Structural mapping coverage reports no blocking binding or relationship-key findings.",
    ),
  ).toBeVisible();
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(
    page.getByRole("heading", { name: "Mapping coverage" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Review and launch validation" }),
  ).toBeVisible();
  await page
    .getByRole("checkbox", {
      name: /I reviewed the immutable rules and accept the fixed local execution policy/,
    })
    .check();
  await page.getByRole("button", { name: "Launch local validation" }).click();

  const handoff = page.getByRole("link", { name: "Review first issues" });
  await expect(handoff).toBeVisible();
  await expect(handoff).toHaveAttribute("href", "/issues");
  await handoff.click();

  await expect(page).toHaveURL(`${freshEngagementOrigin}/issues`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Issues" }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Decisions", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Package", exact: true }),
  ).toBeVisible();
  expect(browserErrors).toEqual([]);
});
