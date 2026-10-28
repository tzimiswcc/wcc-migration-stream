import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import { COMMAND_SCHEMAS, SCHEMAS } from "./contracts";
import {
  decisionCommandResultFixture,
  governedContextFixture,
  interruptedCorrectionFixture,
  interruptedDecisionQueueFixture,
} from "./test/fixtures";
import { errorResponse, mockApi } from "./test/mock-api";

const issueId = "issue-synthetic-001";

function at(path: string): void {
  window.history.replaceState(null, "", path);
}

describe("governed issue-decision workbench", () => {
  beforeEach(() => {
    at("/");
    window.localStorage.clear();
    window.sessionStorage.clear();
  });

  it("keeps WP-004 read-only mode closed by default", async () => {
    at("/decisions");
    const { calls } = mockApi();
    render(<App />);
    expect(
      await screen.findByRole("heading", {
        name: "Decisions is locked",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("link", { name: /^Decisions\b/ }),
    ).not.toBeInTheDocument();
    expect(
      calls.some((call) => call.pathname === "/api/v1/decision-queue"),
    ).toBe(false);
  });

  it("shows a bounded decision queue without broad source or overlay values", async () => {
    at("/decisions");
    mockApi({ "/api/v1/context": governedContextFixture });
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Decision Queue" }),
    ).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Decisions Current" }),
    ).toHaveAttribute("aria-current", "page");
    const queue = await screen.findByRole("region", {
      name: "Governed decision queue",
    });
    expect(within(queue).getByText("CORRECTED")).toBeVisible();
    expect(screen.queryByText("000123")).not.toBeInTheDocument();
    expect(screen.queryByText("SYNTHETIC-123")).not.toBeInTheDocument();
  });

  it("renders exact evidence safely and exposes only the four closed actions", async () => {
    at(`/issues/${issueId}`);
    mockApi({ "/api/v1/context": governedContextFixture });
    const { container } = render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Issue Decision" }),
    ).toBeVisible();
    expect(
      await screen.findByText('<script>alert("not executed")</script>'),
    ).toBeVisible();
    expect(screen.getByText('<img src=x onerror="alert(1)">')).toBeVisible();
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(
      screen.getByRole("heading", { name: "Latest command attempt" }),
    ).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Inspect exact correction attempt" }),
    ).toHaveAttribute("href", "/corrections/overlay-synthetic-002");
    const actions = within(screen.getByLabelText("Issue decisions"));
    expect(
      actions.getAllByRole("link").map((link) => link.textContent),
    ).toEqual(["Correct", "Waive", "Accept as-is", "Exclude row"]);
  });

  it("keeps a correction draft ephemeral and requires review before commit", async () => {
    const user = userEvent.setup();
    at(`/issues/${issueId}/decide/correct`);
    const { requests } = mockApi({
      "/api/v1/context": governedContextFixture,
    });
    render(<App />);
    await screen.findByLabelText("Practitioner identifier");
    await user.type(
      screen.getByLabelText("Practitioner identifier"),
      "practitioner-alpha",
    );
    await user.type(
      screen.getByLabelText("Reason"),
      "Correct the bounded synthetic value.",
    );
    await user.type(
      screen.getByLabelText("Evidence reference (optional)"),
      "evidence-private-alpha",
    );
    await user.type(
      screen.getByLabelText("Proposed overlay text"),
      "PRIVATE-DRAFT-VALUE",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /I understand Correct records a non-destructive overlay/,
      }),
    );
    await user.click(screen.getByRole("button", { name: "Review decision" }));

    expect(
      screen.getByRole("heading", { name: "Confirm governed decision" }),
    ).toBeVisible();
    expect(window.location.href).not.toContain("PRIVATE-DRAFT-VALUE");
    expect(window.location.href).not.toContain("evidence-private-alpha");
    expect(window.localStorage).toHaveLength(0);
    expect(window.sessionStorage).toHaveLength(0);

    await user.click(
      screen.getByRole("button", { name: "Commit immutable decision" }),
    );
    expect(await screen.findByText("Durable decision recorded")).toBeVisible();

    const request = requests.find((item) => item.method === "POST");
    expect(request?.url.pathname).toBe(
      `/api/v1/issues/${issueId}/decisions/correct`,
    );
    expect(request?.headers.get("idempotency-key")).toMatch(/^decision-/);
    expect(request?.headers.get("x-migration-stream-intent")).toBe(
      "governed-decision-v1",
    );
    const body = JSON.parse(request?.body ?? "null") as Record<string, unknown>;
    expect(body.schema_version).toBe(COMMAND_SCHEMAS.corrected);
    expect(body).not.toHaveProperty("idempotency_key");
    expect(body).not.toHaveProperty("source_mutation");
    expect(body.correction).toMatchObject({
      source_file: "objects/alpha.csv",
      source_ordinal: 2,
      entity_id: "SyntheticObject",
      field_id: "object_key",
      operation: "REPLACE_EFFECTIVE_TEXT",
      value: "PRIVATE-DRAFT-VALUE",
    });
  });

  it("reports a stale head without presenting a false commit", async () => {
    const user = userEvent.setup();
    at(`/issues/${issueId}/decide/waive`);
    mockApi({
      "/api/v1/context": governedContextFixture,
      [`POST /api/v1/issues/${issueId}/decisions/waive`]: () =>
        errorResponse(
          {
            schema_version: SCHEMAS.error,
            ok: false,
            error: {
              code: "STALE_DECISION_HEAD",
              message: "The active decision changed before confirmation.",
              request_id: "request-stale-decision",
              retryable: false,
            },
          },
          409,
        ),
    });
    render(<App />);
    await screen.findByLabelText("Practitioner identifier");
    await user.type(
      screen.getByLabelText("Practitioner identifier"),
      "practitioner-alpha",
    );
    await user.type(screen.getByLabelText("Reason"), "Bounded waiver reason.");
    await user.click(
      screen.getByRole("checkbox", {
        name: /I understand Waive keeps the row included/,
      }),
    );
    await user.click(screen.getByRole("button", { name: "Review decision" }));
    await user.click(
      screen.getByRole("button", { name: "Commit immutable decision" }),
    );
    expect(await screen.findByText("STALE_DECISION_HEAD")).toBeVisible();
    expect(
      screen.getByText("Decision was not presented as committed"),
    ).toBeVisible();
    expect(
      screen.queryByText("Durable decision recorded"),
    ).not.toBeInTheDocument();
  });

  it("surfaces an inert latest attempt without broad values or reasons", async () => {
    at("/decisions");
    mockApi({
      "/api/v1/context": governedContextFixture,
      "/api/v1/decision-queue": interruptedDecisionQueueFixture,
    });
    render(<App />);
    expect(await screen.findByText("REEVALUATION_INTERRUPTED")).toBeVisible();
    expect(
      screen.getByRole("link", { name: "Inspect attempt overlay" }),
    ).toHaveAttribute("href", "/corrections/overlay-synthetic-interrupted");
    expect(screen.queryByText("SYNTHETIC-123")).not.toBeInTheDocument();
    expect(
      screen.queryByText("Corrected an exact synthetic text field."),
    ).not.toBeInTheDocument();
  });

  it("renders inert correction diagnostics without granting a decision", async () => {
    at("/corrections/overlay-synthetic-interrupted");
    mockApi({
      "/api/v1/context": governedContextFixture,
      "/api/v1/corrections/overlay-synthetic-interrupted":
        interruptedCorrectionFixture,
    });
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Correction Overlay" }),
    ).toBeVisible();
    expect(await screen.findByText("No effective decision")).toBeVisible();
    expect(screen.getByText("Not applied")).toBeVisible();
    expect(screen.getByText("VALIDATION_CANCELLED")).toBeVisible();
    expect(
      screen.getByText(
        "This diagnostic does not create or reactivate a decision.",
      ),
    ).toBeVisible();
  });

  it.each([
    [
      "correct",
      "I understand Correct records a non-destructive overlay and bounded re-evaluation only. It does not change accepted source evidence or approve a migration result, publication, external transmission, or target load.",
      "acknowledge_not_migration_result_approval",
    ],
    [
      "waive",
      "I understand Waive keeps the row included as a governed exception without changing validation truth, source evidence, or the effective value; excluding the row is a separate decision. It does not approve a migration result, publication, external transmission, or target load.",
      "acknowledge_waiver_limitations",
    ],
    [
      "accept-as-is",
      "I understand Accept as-is retains the exact current effective value and leaves validation truth unchanged. It does not approve a migration result, publication, external transmission, or target load.",
      "acknowledge_validation_truth_unchanged",
    ],
    [
      "exclude-row",
      "I understand Exclude row records evidence only for this exact accepted row in a future candidate. It does not delete source data or approve a migration result, publication, external transmission, or target load.",
      "acknowledge_row_scope",
    ],
  ] as const)(
    "requires and confirms the disposition-specific %s acknowledgement",
    async (action, acknowledgment, commandField) => {
      const user = userEvent.setup();
      at(`/issues/${issueId}/decide/${action}`);
      const { requests } = mockApi({
        "/api/v1/context": governedContextFixture,
      });
      render(<App />);
      await screen.findByLabelText("Practitioner identifier");
      await user.type(
        screen.getByLabelText("Practitioner identifier"),
        "practitioner-acknowledgement",
      );
      await user.type(
        screen.getByLabelText("Reason"),
        `Exercise the exact ${action} acknowledgement.`,
      );
      if (action === "correct") {
        await user.type(
          screen.getByLabelText("Proposed overlay text"),
          "SYNTHETIC-ACKNOWLEDGED-VALUE",
        );
      }
      await user.click(
        screen.getByRole("checkbox", {
          name: acknowledgment,
        }),
      );
      await user.click(screen.getByRole("button", { name: "Review decision" }));

      expect(screen.getByText(acknowledgment)).toBeVisible();
      await user.click(
        screen.getByRole("button", { name: "Commit immutable decision" }),
      );
      expect(
        await screen.findByText("Durable decision recorded"),
      ).toBeVisible();
      const post = requests.find((request) => request.method === "POST");
      const body = JSON.parse(post?.body ?? "null") as Record<string, unknown>;
      expect(body.acknowledge_not_migration_result_approval).toBe(true);
      expect(body[commandField]).toBe(true);
    },
  );

  it("rotates command identity after a definitive failure while preserving the draft", async () => {
    const user = userEvent.setup();
    at(`/issues/${issueId}/decide/waive`);
    let submissions = 0;
    const { requests } = mockApi({
      "/api/v1/context": governedContextFixture,
      [`POST /api/v1/issues/${issueId}/decisions/waive`]: () => {
        submissions += 1;
        return submissions === 1
          ? errorResponse(
              {
                schema_version: SCHEMAS.error,
                ok: false,
                error: {
                  code: "DECISION_INVALID",
                  message: "The bounded synthetic command was rejected.",
                  request_id: "request-definitive-decision",
                  retryable: false,
                },
              },
              400,
            )
          : decisionCommandResultFixture;
      },
    });
    render(<App />);
    await screen.findByLabelText("Practitioner identifier");
    await user.type(
      screen.getByLabelText("Practitioner identifier"),
      "practitioner-preserved",
    );
    await user.type(
      screen.getByLabelText("Reason"),
      "Preserve this bounded draft across a definitive failure.",
    );
    await user.type(
      screen.getByLabelText("Evidence reference (optional)"),
      "evidence-preserved",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /I understand Waive keeps the row included/,
      }),
    );
    await user.click(screen.getByRole("button", { name: "Review decision" }));
    await user.click(
      screen.getByRole("button", { name: "Commit immutable decision" }),
    );
    expect(await screen.findByText("DECISION_INVALID")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Retry same command identity" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Return to edit" }));
    expect(screen.getByLabelText("Practitioner identifier")).toHaveValue(
      "practitioner-preserved",
    );
    expect(screen.getByLabelText("Reason")).toHaveValue(
      "Preserve this bounded draft across a definitive failure.",
    );
    expect(screen.getByLabelText("Evidence reference (optional)")).toHaveValue(
      "evidence-preserved",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /I understand Waive keeps the row included/,
      }),
    );
    await user.click(screen.getByRole("button", { name: "Review decision" }));
    await user.click(
      screen.getByRole("button", { name: "Commit immutable decision" }),
    );
    expect(await screen.findByText("Durable decision recorded")).toBeVisible();

    const posts = requests.filter((request) => request.method === "POST");
    expect(posts).toHaveLength(2);
    const firstKey = posts[0].headers.get("idempotency-key");
    const secondKey = posts[1].headers.get("idempotency-key");
    expect(firstKey).toMatch(/^decision-/);
    expect(secondKey).toMatch(/^decision-/);
    expect(secondKey).not.toBe(firstKey);
  });

  it("preserves command identity only for an uncertain same-command retry", async () => {
    const user = userEvent.setup();
    at(`/issues/${issueId}/decide/waive`);
    let submissions = 0;
    const { requests } = mockApi({
      "/api/v1/context": governedContextFixture,
      [`POST /api/v1/issues/${issueId}/decisions/waive`]: () => {
        submissions += 1;
        if (submissions === 1) {
          throw new TypeError("synthetic uncertain transport failure");
        }
        return decisionCommandResultFixture;
      },
    });
    render(<App />);
    await screen.findByLabelText("Practitioner identifier");
    await user.type(
      screen.getByLabelText("Practitioner identifier"),
      "practitioner-uncertain-retry",
    );
    await user.type(
      screen.getByLabelText("Reason"),
      "Retry these exact command bytes after an uncertain result.",
    );
    await user.click(
      screen.getByRole("checkbox", {
        name: /I understand Waive keeps the row included/,
      }),
    );
    await user.click(screen.getByRole("button", { name: "Review decision" }));
    await user.click(
      screen.getByRole("button", { name: "Commit immutable decision" }),
    );
    expect(await screen.findByText("NETWORK_UNCERTAIN")).toBeVisible();

    await user.click(
      screen.getByRole("button", { name: "Retry same command identity" }),
    );
    expect(
      screen.getByRole("heading", { name: "Confirm governed decision" }),
    ).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: "Commit immutable decision" }),
    );
    expect(await screen.findByText("Durable decision recorded")).toBeVisible();

    const posts = requests.filter((request) => request.method === "POST");
    expect(posts).toHaveLength(2);
    const firstKey = posts[0].headers.get("idempotency-key");
    const secondKey = posts[1].headers.get("idempotency-key");
    expect(firstKey).toMatch(/^decision-/);
    expect(secondKey).toBe(firstKey);
    expect(posts[1].body).toBe(posts[0].body);
  });

  it("preserves active and superseded history as separate evidence", async () => {
    at(`/issues/${issueId}/history`);
    mockApi({ "/api/v1/context": governedContextFixture });
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Decision History" }),
    ).toBeVisible();
    expect(await screen.findByText("ACTIVE")).toBeVisible();
    expect(screen.getByText("SUPERSEDED")).toBeVisible();
    expect(screen.getByText("Earlier synthetic disposition.")).toBeVisible();
  });

  it("supports keyboard action navigation without accessibility violations", async () => {
    const user = userEvent.setup();
    at(`/issues/${issueId}`);
    mockApi({ "/api/v1/context": governedContextFixture });
    render(<App />);
    await screen.findByRole("heading", { name: "Issue Decision" });
    const waive = await screen.findByRole("link", { name: "Waive" });
    waive.focus();
    expect(waive).toHaveFocus();
    const result = await axe.run(document.body);
    expect(result.violations).toEqual([]);
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(window.location.pathname).toContain("/decide/waive"),
    );
    expect(
      await screen.findByRole("heading", { level: 1, name: "Waive" }),
    ).toBeVisible();
  });
});
