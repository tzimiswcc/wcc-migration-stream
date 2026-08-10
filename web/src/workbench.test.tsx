import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { beforeEach, describe, expect, it } from "vitest";
import App from "./App";
import { SCHEMAS } from "./contracts";
import { mockApi } from "./test/mock-api";
import {
  governedContextFixture,
  operationsFixture,
  publicationContextFixture,
  reconciliationFixture,
  routeFixtures,
} from "./test/fixtures";

function at(path: string): void {
  window.history.replaceState(null, "", path);
}

const resultApprovalContextFixture = {
  ...publicationContextFixture,
  schema_version: SCHEMAS.resultApprovalContext,
  capabilities: {
    ...publicationContextFixture.capabilities,
    publication: false,
  },
};

describe("practitioner workbench", () => {
  beforeEach(() => at("/"));

  it("renders the startup scope, read-only notice, and server integrity truth", async () => {
    mockApi();
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeVisible();
    expect(screen.getByText("Migration Stream")).toBeVisible();
    expect(screen.getByText("Migration Journey")).toBeVisible();
    expect(screen.queryByText("MS")).not.toBeInTheDocument();
    expect(screen.getByText("Synthetic readiness workspace")).toBeVisible();
    expect(screen.getByText("Read-only local evidence")).toBeVisible();
    expect(await screen.findByText("NOT_CHECKED")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /approve|run|publish/i }),
    ).not.toBeInTheDocument();
  });

  it("uses history-aware links and responds predictably to popstate", async () => {
    const user = userEvent.setup();
    mockApi();
    render(<App />);
    await screen.findByRole("heading", { name: "Overview" });
    await user.click(screen.getByRole("link", { name: "Files" }));
    expect(await screen.findByRole("heading", { name: "Files" })).toBeVisible();
    expect(window.location.pathname).toBe("/files");
    window.history.replaceState(null, "", "/");
    window.dispatchEvent(new PopStateEvent("popstate"));
    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeVisible();
  });

  it("pairs file pages with authoritative per-file reconciliation and cursors", async () => {
    const user = userEvent.setup();
    at("/files?page_size=1");
    const { calls } = mockApi();
    render(<App />);
    expect(await screen.findByText("objects/alpha.csv")).toBeVisible();
    expect(screen.getByText(/source 2 · stored 2 · pending 0/)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(window.location.search).toContain("cursor=files-next-opaque");
    expect(window.location.search).toContain(
      "reconciliation_cursor=reconciliation-next-opaque",
    );
    await waitFor(() =>
      expect(
        calls.some(
          (call) =>
            call.pathname === "/api/v1/reconciliation" &&
            call.searchParams.get("cursor") === "reconciliation-next-opaque",
        ),
      ).toBe(true),
    );
    const size = screen.getByLabelText("Page size");
    await user.clear(size);
    await user.type(size, "2");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    expect(window.location.search).not.toContain("cursor=");
    expect(window.location.search).not.toContain("reconciliation_cursor=");
  });

  it("does not advance when paired file cursors are incomplete", async () => {
    at("/files?page_size=1");
    mockApi({
      "/api/v1/reconciliation": {
        ...reconciliationFixture,
        page: { page_size: 1, returned: 1, next_cursor: null },
      },
    });
    render(<App />);
    expect(
      await screen.findByText(/Paired server pagination is incomplete/),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Next page" })).toBeDisabled();
  });

  it("keeps profiling samples off until an explicit practitioner action", async () => {
    const user = userEvent.setup();
    at("/profiling");
    mockApi();
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Profiling" }),
    ).toBeVisible();
    expect(screen.queryByText("000123")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Request samples" }));
    expect(await screen.findByText("000123")).toBeVisible();
    expect(window.location.search).toContain("include_samples=true");
  });

  it("shows profile provenance separately from local authority", async () => {
    at("/profile");
    mockApi();
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Technical provenance" }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Local authority" }),
    ).toBeVisible();
    expect(screen.getByText("compatibilityDeclarations")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /approve/i }),
    ).not.toBeInTheDocument();
  });

  it("preserves mapping categories and lineage labels without an editor", async () => {
    at("/mapping");
    mockApi();
    render(<App />);
    expect(await screen.findByText("MAPPED")).toBeVisible();
    expect(screen.getByText("UNCOVERED_REQUIRED_FIELD")).toBeVisible();
    expect(screen.getByText("DIRECT")).toBeVisible();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("applies allow-listed issue filters and a bounded page size in URL state", async () => {
    const user = userEvent.setup();
    at("/issues?page_size=25");
    const { calls } = mockApi();
    render(<App />);
    await screen.findByRole("heading", { name: "Issues" });
    await user.selectOptions(screen.getByLabelText("Severity"), "ERROR");
    await user.type(screen.getByLabelText("Rule"), "required.object_key");
    const size = screen.getByLabelText("Page size");
    await user.clear(size);
    await user.type(size, "40");
    await user.click(screen.getByRole("button", { name: "Apply filters" }));
    expect(window.location.search).toContain("severity=ERROR");
    expect(window.location.search).toContain("rule_id=required.object_key");
    expect(window.location.search).toContain("page_size=40");
    await waitFor(() =>
      expect(
        calls.some(
          (call) =>
            call.pathname === "/api/v1/issues" &&
            call.searchParams.get("page_size") === "40",
        ),
      ).toBe(true),
    );
  });

  it("keeps received source values absent from broad issue pages", async () => {
    at("/issues");
    mockApi();
    render(<App />);
    expect(await screen.findByText("REQUIRED_TEXT_EMPTY")).toBeVisible();
    expect(screen.queryByText("000123")).not.toBeInTheDocument();
    expect(screen.queryByText("1e10")).not.toBeInTheDocument();
  });

  it.each([
    {
      mode: "governed decisions",
      context: governedContextFixture,
      labels: [
        "Setup / Workspace",
        "Destination",
        "Source",
        "Mapping",
        "Validate",
        "Issues",
        "Decisions",
        "Reconciliation",
      ],
    },
    {
      mode: "result approval",
      context: resultApprovalContextFixture,
      labels: [
        "Setup / Workspace",
        "Destination",
        "Source",
        "Mapping",
        "Validate",
        "Issues",
        "Decisions",
        "Reconciliation",
      ],
    },
    {
      mode: "readiness packages",
      context: publicationContextFixture,
      labels: [
        "Setup / Workspace",
        "Destination",
        "Source",
        "Mapping",
        "Validate",
        "Issues",
        "Decisions",
        "Reconciliation",
        "Package",
      ],
    },
  ])(
    "keeps the server-derived journey sequence truthful in $mode mode",
    async ({ context, labels }) => {
      mockApi({ "/api/v1/context": context });
      render(<App />);
      await screen.findByRole("heading", { name: "Overview" });
      const navigation = screen.getByRole("navigation", {
        name: "Migration journey",
      });
      expect(
        within(navigation)
          .getAllByRole("link")
          .map((link) => link.getAttribute("aria-label")),
      ).toEqual(labels);
    },
  );

  it("opens one exact row and preserves text safely without HTML execution", async () => {
    at("/rows/2?source_file=objects%2Falpha.csv");
    mockApi();
    const { container } = render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Row Detail" }),
    ).toBeVisible();
    expect(
      await screen.findByRole("heading", { name: "Immutable received text" }),
    ).toBeVisible();
    expect(
      await screen.findByRole("heading", { name: "Projection and lineage" }),
    ).toBeVisible();
    expect(screen.getAllByText("000123")).toHaveLength(2);
    expect(screen.getByText("1e10")).toBeVisible();
    expect(screen.getByText("2026-07-22")).toBeVisible();
    expect(screen.getByText("Ωμέγα")).toBeVisible();
    expect(
      screen.getByText('<script>alert("not executed")</script>'),
    ).toBeVisible();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByText("DIRECT")).toBeVisible();
    expect(screen.getByText("CONSTANT")).toBeVisible();
    expect(
      screen.queryByRole("link", { name: "Open governed context" }),
    ).not.toBeInTheDocument();
  });

  it("links exact Row Detail to its existing governed issue without an extra read", async () => {
    const user = userEvent.setup();
    at("/rows/2?source_file=objects%2Falpha.csv");
    const { calls } = mockApi({
      "/api/v1/context": governedContextFixture,
    });
    render(<App />);

    const governedContext = await screen.findByRole("link", {
      name: "Open governed context",
    });
    expect(governedContext).toHaveAttribute(
      "href",
      "/issues/issue-synthetic-001",
    );
    expect(
      calls.filter((call) => call.pathname.startsWith("/api/v1/rows/")),
    ).toHaveLength(1);
    expect(
      calls.some((call) => call.pathname.endsWith("/decision-detail")),
    ).toBe(false);

    await user.click(governedContext);
    expect(window.location.pathname).toBe("/issues/issue-synthetic-001");
    expect(
      await screen.findByRole("heading", { name: "Issue Decision" }),
    ).toBeVisible();
  });

  it("rejects zero as an invalid exact-row ordinal without a row API read", async () => {
    at("/rows/0?source_file=objects%2Falpha.csv");
    const { calls } = mockApi();
    render(<App />);
    expect(await screen.findByText("Invalid row selection")).toBeVisible();
    expect(
      calls.some((call) => call.pathname.startsWith("/api/v1/rows/")),
    ).toBe(false);
  });

  it("shows reconciliation closure and validation coverage without approval", async () => {
    at("/reconciliation");
    mockApi();
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Aggregate reconciliation" }),
    ).toBeVisible();
    expect(screen.getByText("objects/alpha.csv")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /publish|approve/i }),
    ).not.toBeInTheDocument();
  });

  it("offers only allow-listed operational filters and suppresses internal identifiers", async () => {
    const user = userEvent.setup();
    at("/operations");
    const { calls } = mockApi();
    render(<App />);
    expect(
      (await screen.findAllByText("CLEANUP_PENDING")).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText("internal-cleanup-001")).not.toBeInTheDocument();
    const namespace = screen.getByLabelText("Namespace");
    const state = screen.getByLabelText("State");
    expect(
      within(namespace)
        .getAllByRole("option")
        .map((option) => option.getAttribute("value")),
    ).toEqual(["", "packages", "metadata", "catalogues"]);
    expect(
      within(state)
        .getAllByRole("option")
        .map((option) => option.getAttribute("value")),
    ).toEqual([
      "",
      "PLANNED",
      "QUARANTINED",
      "CLEANUP_PENDING",
      "CLEANED",
      "CONFLICT",
    ]);
    await user.selectOptions(namespace, "packages");
    await user.selectOptions(state, "CLEANUP_PENDING");
    await user.click(screen.getByRole("button", { name: "Apply filters" }));
    await waitFor(() =>
      expect(
        calls.some(
          (call) =>
            call.pathname === "/api/v1/operations" &&
            call.searchParams.get("namespace") === "packages" &&
            call.searchParams.get("state") === "CLEANUP_PENDING",
        ),
      ).toBe(true),
    );
  });

  it("supports keyboard navigation with visible semantic landmarks", async () => {
    const user = userEvent.setup();
    mockApi();
    render(<App />);
    await screen.findByText("NOT_CHECKED");
    const files = screen.getByRole("link", { name: "Files" });
    files.focus();
    expect(files).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "Files" })).toBeVisible();
    expect(screen.getByRole("main")).toHaveAttribute(
      "data-layout",
      "responsive",
    );
    expect(
      screen.getAllByText("This table scrolls horizontally in a narrow layout.")
        .length,
    ).toBeGreaterThan(0);
  });

  it("has no automated accessibility violations on the bounded overview", async () => {
    mockApi();
    render(<App />);
    await screen.findByRole("heading", { name: "Overview" });
    const result = await axe.run(document.body);
    expect(result.violations).toEqual([]);
  });

  it("keeps mock evidence synthetic and does not expose cleanup diagnostics", async () => {
    at("/operations");
    mockApi({
      "/api/v1/operations": {
        ...operationsFixture,
        items: operationsFixture.items.map((item) => ({
          ...item,
          last_error_code: "DELETE_DEFERRED",
        })),
      },
    });
    render(<App />);
    expect(await screen.findByText("DELETE_DEFERRED")).toBeVisible();
    expect(document.body.textContent).not.toMatch(
      /tombstone[/\\]|absolute path|diagnostic message/i,
    );
    expect(routeFixtures["/api/v1/issues"]).toBeDefined();
  });
});
