import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { EmptyState, Pagination, Status } from "./ui";

const page = { page_size: 25, returned: 1, next_cursor: null };

describe("bounded presentation helpers", () => {
  beforeEach(() => window.history.replaceState(null, "", "/issues?cursor=old"));

  it("does not infer domain meaning from generic status labels", () => {
    render(
      <>
        <Status value="COMPLETE" />
        <Status value={false} />
      </>,
    );
    expect(screen.getByText("COMPLETE")).toHaveClass("status--neutral");
    expect(screen.getByText("NO")).toHaveClass("status--neutral");
  });

  it("uses a deterministic internal first-page link", async () => {
    const user = userEvent.setup();
    render(
      <Pagination
        page={page}
        firstHref="/issues?page_size=25"
        nextHref={null}
      />,
    );
    const first = screen.getByRole("link", { name: "First page" });
    expect(first).toHaveAttribute("href", "/issues?page_size=25");
    await user.click(first);
    expect(window.location.pathname).toBe("/issues");
    expect(window.location.search).toBe("?page_size=25");
  });

  it("renders first-page navigation as inactive without a safe target", () => {
    render(<Pagination page={page} firstHref={null} nextHref={null} />);
    expect(screen.getByRole("link", { name: "First page" })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("keeps the default empty-state title and accepts a bounded override", () => {
    const { rerender } = render(
      <EmptyState>No accepted file exists.</EmptyState>,
    );
    expect(screen.getByText("No evidence returned")).toBeVisible();

    rerender(
      <EmptyState title="No local packages generated">
        Generated packages will appear here.
      </EmptyState>,
    );
    expect(screen.getByText("No local packages generated")).toBeVisible();
    expect(screen.queryByText("No evidence returned")).not.toBeInTheDocument();
  });
});
