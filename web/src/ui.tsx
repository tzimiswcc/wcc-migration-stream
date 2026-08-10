import { Component, type ReactNode } from "react";
import type { ReadState } from "./api";
import type { JsonValue, PageInfo } from "./contracts";
import { AppLink, navigate } from "./router";

export class SafeRenderBoundary extends Component<
  { children: ReactNode },
  { failed: boolean }
> {
  override state = { failed: false };

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }

  override render(): ReactNode {
    if (this.state.failed) {
      return (
        <div className="state-panel state-panel--error" role="alert">
          <div>
            <p className="eyebrow">INVALID_RESPONSE</p>
            <strong>Evidence could not be displayed safely</strong>
            <p>
              The response was withheld because it could not be rendered as a
              supported local contract.
            </p>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

export function ReadBoundary<T>({
  state,
  retry,
  children,
}: {
  state: ReadState<T>;
  retry: () => void;
  children: (data: T) => ReactNode;
}) {
  if (state.status === "loading") {
    return (
      <div className="state-panel" role="status" aria-live="polite">
        <span className="spinner" aria-hidden="true" />
        <div>
          <strong>Loading read-only evidence</strong>
          <p>Waiting for the selected local workspace.</p>
        </div>
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <div className="state-panel state-panel--error" role="alert">
        <div>
          <p className="eyebrow">{state.error.code}</p>
          <strong>Evidence is unavailable</strong>
          <p>{state.error.message}</p>
          {state.error.requestId ? (
            <p className="supporting">Request {state.error.requestId}</p>
          ) : null}
          <button type="button" className="button" onClick={retry}>
            Retry safe read
          </button>
        </div>
      </div>
    );
  }
  return <>{children(state.data)}</>;
}

export function PageHeader({
  eyebrow,
  title,
  description,
  action,
}: {
  eyebrow: string;
  title: string;
  description: string;
  action?: ReactNode;
}) {
  return (
    <header className="page-header">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h1>{title}</h1>
        <p className="lede">{description}</p>
      </div>
      {action ? <div className="page-header__action">{action}</div> : null}
    </header>
  );
}

export function Card({
  title,
  eyebrow,
  children,
  className = "",
}: {
  title?: string;
  eyebrow?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`card ${className}`.trim()}>
      {eyebrow ? <p className="eyebrow">{eyebrow}</p> : null}
      {title ? <h2>{title}</h2> : null}
      {children}
    </section>
  );
}

export function Status({ value }: { value: string | boolean }) {
  const label = typeof value === "boolean" ? (value ? "YES" : "NO") : value;
  return <span className="status status--neutral">{label}</span>;
}

export function DefinitionGrid({
  items,
}: {
  items: Array<{ label: string; value: ReactNode }>;
}) {
  return (
    <dl className="definition-grid">
      {items.map((item) => (
        <div key={item.label}>
          <dt>{item.label}</dt>
          <dd>{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Hash({ children }: { children: string | null }) {
  return children ? (
    <code className="hash" title={children}>
      {children}
    </code>
  ) : (
    <span className="muted">Not available</span>
  );
}

export function Value({ children }: { children: ReactNode }) {
  return <span className="evidence-value">{children ?? "—"}</span>;
}

export function EmptyState({
  title = "No evidence returned",
  children,
}: {
  title?: string;
  children: ReactNode;
}) {
  return (
    <div className="empty-state" role="status">
      <strong>{title}</strong>
      <p>{children}</p>
    </div>
  );
}

export function TableShell({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="table-shell" role="region" aria-label={label} tabIndex={0}>
      <p className="narrow-layout-marker">
        This table scrolls horizontally in a narrow layout.
      </p>
      {children}
    </div>
  );
}

export function Pagination({
  page,
  firstHref,
  nextHref,
}: {
  page: PageInfo;
  firstHref: string | null;
  nextHref: string | null;
}) {
  return (
    <nav className="pagination" aria-label="Evidence pagination">
      {firstHref ? (
        <AppLink className="button button--secondary" href={firstHref}>
          First page
        </AppLink>
      ) : (
        <span
          className="button button--secondary"
          aria-disabled="true"
          role="link"
        >
          First page
        </span>
      )}
      <span aria-live="polite">
        Returned {page.returned} · page size {page.page_size}
      </span>
      <button
        type="button"
        className="button button--secondary"
        disabled={!nextHref}
        onClick={() => nextHref && navigate(nextHref)}
      >
        Next page
      </button>
    </nav>
  );
}

export function StructuredValue({ value }: { value: JsonValue }) {
  if (value === null) return <span className="muted">null</span>;
  if (typeof value === "string") return <Value>{value}</Value>;
  if (typeof value === "number" || typeof value === "boolean") {
    return <Value>{String(value)}</Value>;
  }
  if (Array.isArray(value)) {
    if (value.length === 0) return <span className="muted">Empty list</span>;
    return (
      <ol className="structured-list">
        {value.map((item, index) => (
          <li key={index}>
            <StructuredValue value={item} />
          </li>
        ))}
      </ol>
    );
  }
  const entries = Object.entries(value);
  if (entries.length === 0) return <span className="muted">Empty object</span>;
  return (
    <dl className="structured-object">
      {entries.map(([key, item]) => (
        <div key={key}>
          <dt>{key}</dt>
          <dd>
            <StructuredValue value={item} />
          </dd>
        </div>
      ))}
    </dl>
  );
}
