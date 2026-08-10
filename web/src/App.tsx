import { useEffect } from "react";
import { useRead } from "./api";
import { AUTHORITY_CHANGED_EVENT } from "./api";
import {
  SCHEMAS,
  canApproveMigrationResult,
  canGovernIssueDecisions,
  canPublishReadiness,
  isEngagementContext,
  type ApplicationContext,
  type Journey,
  type WorkbenchContext,
} from "./contracts";
import { EngagementShell } from "./engagement-shell";
import { DestinationPage } from "./destination-workshop";
import {
  CorrectionOverlayPage,
  DecisionActionPage,
  DecisionCapabilityUnavailablePage,
  DecisionHistoryPage,
  DecisionIssuePage,
  DecisionQueuePage,
  DecisionResultPage,
} from "./decision-pages";
import {
  FilesPage,
  IssuesPage,
  MappingPage,
  NotFoundPage,
  OperationsPage,
  OverviewPage,
  ProfilePage,
  ProfilingPage,
  ReconciliationPage,
  RowDetailPage,
  ValidationPage,
} from "./pages";
import {
  PublicationCapabilityUnavailablePage,
  PublicationEvidencePage,
  PublicationWorkbenchPage,
} from "./publication-workbench";
import {
  ResultApprovalCapabilityUnavailablePage,
  ResultApprovalPage,
} from "./reconciliation-approval-pages";
import { AppLink, useLocation } from "./router";
import { WorkspaceHeaderIdentity } from "./object-workspace";
import { TourComparisonPage } from "./tour2-pages";
import {
  JourneyRail,
  JourneyStageUnavailable,
  journeyStageForLocation,
} from "./journey-rail";
import { StageHelpPanel } from "./stage-help";
import { ReadBoundary, SafeRenderBoundary } from "./ui";

function decisionRoute(location: URL) {
  if (location.pathname === "/decisions") {
    return <DecisionQueuePage location={location} />;
  }
  let match = /^\/decisions\/([^/]+)$/.exec(location.pathname);
  if (match) return <DecisionResultPage decisionId={match[1]} />;
  match = /^\/corrections\/([^/]+)$/.exec(location.pathname);
  if (match) return <CorrectionOverlayPage overlayId={match[1]} />;
  match = /^\/issues\/([^/]+)\/history$/.exec(location.pathname);
  if (match)
    return <DecisionHistoryPage issueId={match[1]} location={location} />;
  match = /^\/issues\/([^/]+)\/decide\/([^/]+)$/.exec(location.pathname);
  if (match)
    return <DecisionActionPage issueId={match[1]} actionName={match[2]} />;
  match = /^\/issues\/([^/]+)$/.exec(location.pathname);
  if (match) return <DecisionIssuePage issueId={match[1]} />;
  return null;
}

function decodeRouteSegment(value: string): string | null {
  try {
    const decoded = decodeURIComponent(value);
    return /^[a-zA-Z0-9][a-zA-Z0-9._:-]{0,255}$/.test(decoded) ? decoded : null;
  } catch {
    return null;
  }
}

function route(location: URL, context: WorkbenchContext, journey: Journey) {
  const journeyStage = journeyStageForLocation(journey, location);
  if (journeyStage?.status === "LOCKED") {
    return <JourneyStageUnavailable stage={journeyStage} />;
  }
  const publicationDetail = /^\/publication\/([^/]+)$/.exec(location.pathname);
  if (publicationDetail) {
    const publicationId = decodeRouteSegment(publicationDetail[1]);
    if (!publicationId) return <NotFoundPage />;
    return canPublishReadiness(context) ? (
      <PublicationEvidencePage publicationId={publicationId} />
    ) : (
      <PublicationCapabilityUnavailablePage />
    );
  }
  if (location.pathname === "/result-approval") {
    return canApproveMigrationResult(context) ? (
      <ResultApprovalPage
        location={location}
        publicationAvailable={canPublishReadiness(context)}
      />
    ) : (
      <ResultApprovalCapabilityUnavailablePage />
    );
  }
  const governedRoute = decisionRoute(location);
  if (governedRoute) {
    return canGovernIssueDecisions(context) ? (
      governedRoute
    ) : (
      <DecisionCapabilityUnavailablePage />
    );
  }
  switch (location.pathname) {
    case "/":
    case "/setup":
      return <OverviewPage />;
    case "/files":
      return <FilesPage location={location} />;
    case "/profiling":
      return <ProfilingPage location={location} />;
    case "/profile":
      return <ProfilePage />;
    case "/destination":
      return <DestinationPage />;
    case "/mapping":
      return <MappingPage location={location} />;
    case "/validation":
      return (
        <ValidationPage
          engagementBound={journey.binding.engagementId !== null}
        />
      );
    case "/comparison":
      return <TourComparisonPage entityCount={journey.binding.entityCount} />;
    case "/issues":
      return (
        <IssuesPage
          location={location}
          decisionWritesEnabled={canGovernIssueDecisions(context)}
          engagementBound={journey.binding.engagementId !== null}
        />
      );
    case "/reconciliation":
      return <ReconciliationPage location={location} />;
    case "/publication":
      return canPublishReadiness(context) ? (
        <PublicationWorkbenchPage />
      ) : (
        <PublicationCapabilityUnavailablePage />
      );
    case "/operations":
      return <OperationsPage location={location} />;
    default: {
      const row = /^\/rows\/(\d+)$/.exec(location.pathname);
      if (row)
        return (
          <RowDetailPage
            location={location}
            ordinal={Number(row[1])}
            governedDecisionsEnabled={canGovernIssueDecisions(context)}
          />
        );
      return <NotFoundPage />;
    }
  }
}

function Workbench({
  context,
  journey,
}: {
  context: WorkbenchContext;
  journey: Journey;
}) {
  const location = useLocation();
  const governed = canGovernIssueDecisions(context);
  const resultApproval = canApproveMigrationResult(context);
  const publication = canPublishReadiness(context);
  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        Skip to main content
      </a>
      <header className="app-header">
        <div className="brand-block">
          <div>
            <p>Migration Stream</p>
            <strong>Migration Journey</strong>
          </div>
        </div>
        <WorkspaceHeaderIdentity status={journey.workspace} />
      </header>
      <aside
        className="read-only-banner"
        aria-label={
          governed ? "Governed decision scope notice" : "Read-only scope notice"
        }
      >
        <strong>
          {governed
            ? resultApproval
              ? "Governed local reconciliation and result evidence"
              : "Governed local issue decisions"
            : "Read-only local evidence"}
        </strong>
        <span>
          {governed
            ? resultApproval
              ? publication
                ? "Accepted source, canonical history, validation truth, and profile authority remain immutable; governed local package generation remains eligibility-gated, creates no external transmission, and target-system capabilities remain disabled."
                : "Accepted source, canonical history, validation truth, and profile authority remain immutable; local package generation, external transmission, and target-system capabilities remain disabled."
              : "Source, original canonical projection, validation truth, reconciliation, and profile authority remain immutable."
            : "No action here changes source, authority, validation, reconciliation, or cleanup state."}
        </span>
      </aside>
      <JourneyRail journey={journey} location={location} />
      <StageHelpPanel journey={journey} location={location} />
      <nav className="supporting-nav" aria-label="Supporting workbench views">
        <AppLink href="/files" current={location.pathname === "/files"}>
          Files
        </AppLink>
        <AppLink href="/profiling" current={location.pathname === "/profiling"}>
          Profiling detail
        </AppLink>
        {resultApproval ? (
          <AppLink
            href="/result-approval"
            current={location.pathname === "/result-approval"}
          >
            Local Result Approval
          </AppLink>
        ) : null}
        <AppLink
          href="/operations"
          current={location.pathname === "/operations"}
        >
          Operations
        </AppLink>
      </nav>
      <main id="main-content" className="main-content" data-layout="responsive">
        {location.pathname.startsWith("/rows/") ? (
          <nav className="breadcrumbs" aria-label="Breadcrumb">
            <AppLink href="/issues">Issues</AppLink>
            <span aria-hidden="true">/</span>
            <span aria-current="page">Row Detail</span>
          </nav>
        ) : null}
        <SafeRenderBoundary key={`${location.pathname}${location.search}`}>
          {route(location, context, journey)}
        </SafeRenderBoundary>
      </main>
      <footer className="app-footer">
        <p>
          <strong>
            {governed
              ? resultApproval
                ? "Bounded governed local-evidence surface."
                : "Bounded governed-decision surface."
              : "Read-only practitioner surface."}
          </strong>{" "}
          Server contracts remain authority.
        </p>
        <p>
          {context.supported_contracts.length} contract versions exposed by the
          selected local process.
        </p>
      </footer>
    </div>
  );
}

function ApplicationSurface({
  context,
  journey,
}: {
  context: ApplicationContext;
  journey: Journey;
}) {
  if (isEngagementContext(context)) {
    return <EngagementShell context={context} journey={journey} />;
  }
  return <Workbench context={context} journey={journey} />;
}

function authorityRefreshesContext(event: Event): boolean {
  if (!(event instanceof CustomEvent)) return true;
  const detail: unknown = event.detail;
  return !(
    typeof detail === "object" &&
    detail !== null &&
    "refreshContext" in detail &&
    detail.refreshContext === false
  );
}

export default function App() {
  const context = useRead<ApplicationContext>(
    "/api/v1/context",
    SCHEMAS.context,
  );
  const journey = useRead<Journey>("/api/v1/journey", SCHEMAS.journey);
  const retryContext = context.retry;
  const retryJourney = journey.retry;
  useEffect(() => {
    const refreshAuthority = (event: Event) => {
      if (authorityRefreshesContext(event)) retryContext();
      retryJourney();
    };
    window.addEventListener(AUTHORITY_CHANGED_EVENT, refreshAuthority);
    return () =>
      window.removeEventListener(AUTHORITY_CHANGED_EVENT, refreshAuthority);
  }, [retryContext, retryJourney]);
  return (
    <ReadBoundary {...context}>
      {(data) => (
        <ReadBoundary {...journey}>
          {(journeyData) => (
            <ApplicationSurface context={data} journey={journeyData} />
          )}
        </ReadBoundary>
      )}
    </ReadBoundary>
  );
}
