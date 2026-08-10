import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import { SCHEMAS } from "./contracts";
import { errorResponse, mockApi } from "./test/mock-api";
import {
  contextFixture,
  issuesFixture,
  staleCursorError,
  validationFixture,
} from "./test/fixtures";

function at(path: string): void {
  window.history.replaceState(null, "", path);
}

describe("explicit read states", () => {
  beforeEach(() => at("/"));

  it("announces loading while a read is outstanding", async () => {
    mockApi({
      "/api/v1/job": () => new Promise(() => undefined),
    });
    render(<App />);
    await waitFor(() =>
      expect(screen.getByText("Loading read-only evidence")).toBeVisible(),
    );
  });

  it("renders a bounded empty page explicitly", async () => {
    at("/files");
    mockApi({
      "/api/v1/files": {
        schema_version: SCHEMAS.files,
        scope: { job_id: "job-synthetic-004", source_hash: "1".repeat(64) },
        items: [],
        page: { page_size: 50, returned: 0, next_cursor: null },
        canonical_hash: "2".repeat(64),
      },
      "/api/v1/reconciliation": {
        schema_version: SCHEMAS.reconciliation,
        scope: {
          job_id: "job-synthetic-004",
          source_hash: "1".repeat(64),
          validation_run_id: null,
        },
        aggregate: {
          source_records: 0,
          stored_records: 0,
          pending_records: 0,
          closed: false,
          validation_rows_processed: null,
          validation_issue_count: null,
        },
        items: [],
        page: { page_size: 50, returned: 0, next_cursor: null },
        canonical_hash: "3".repeat(64),
      },
    });
    render(<App />);
    expect(
      await screen.findByText("No accepted file exists on this page."),
    ).toBeVisible();
  });

  it("shows a safe transport error and request identifier", async () => {
    mockApi({
      "/api/v1/job": errorResponse(staleCursorError),
    });
    render(<App />);
    expect(await screen.findByText("STALE_CURSOR")).toBeVisible();
    expect(screen.getByText("Request request-synthetic-stale")).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Retry safe read" }),
    ).toBeEnabled();
  });

  it("shows a contract schema mismatch instead of rendering data", async () => {
    mockApi({
      "/api/v1/job": { ...contextFixture, schema_version: SCHEMAS.context },
    });
    render(<App />);
    expect(await screen.findByText("SCHEMA_MISMATCH")).toBeVisible();
    expect(
      screen.queryByText("Accepted evidence counts"),
    ).not.toBeInTheDocument();
  });

  it.each(["RUNNING", "FAILED", "INTERRUPTED", "BOUND_EXCEEDED"])(
    "keeps the %s validation lifecycle distinct from complete",
    async (state) => {
      at("/validation");
      mockApi({
        "/api/v1/validation": {
          ...validationFixture,
          data: {
            ...validationFixture.data,
            state,
            result_hash: null,
            completed_at: null,
          },
        },
      });
      render(<App />);
      expect(
        await screen.findByRole("heading", { name: "Validation" }),
      ).toBeVisible();
      expect((await screen.findAllByText(state)).length).toBeGreaterThan(0);
      expect(
        screen.getByText((_, element) =>
          Boolean(
            element?.classList.contains("notice--attention") &&
            element.textContent?.includes("It is not displayed as complete."),
          ),
        ),
      ).toBeVisible();
    },
  );

  it("makes a stale issue cursor explicit", async () => {
    at("/issues?cursor=stale");
    mockApi({
      "/api/v1/issues": (url: URL) =>
        url.searchParams.get("cursor") === "stale"
          ? errorResponse(staleCursorError)
          : issuesFixture,
    });
    render(<App />);
    expect(await screen.findByText("STALE_CURSOR")).toBeVisible();
  });

  it("fails context startup when the server does not assert read-only", async () => {
    mockApi({
      "/api/v1/context": { ...contextFixture, read_only: false },
    });
    render(<App />);
    await waitFor(() =>
      expect(screen.getByText("INVALID_CONTEXT")).toBeVisible(),
    );
    expect(
      screen.queryByRole("navigation", { name: "Practitioner views" }),
    ).not.toBeInTheDocument();
  });
});
