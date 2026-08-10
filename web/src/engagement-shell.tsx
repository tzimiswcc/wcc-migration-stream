import type { EngagementContext, Journey } from "./contracts";
import {
  EngagementMappingContractPage,
  EngagementSetupPage,
  EngagementSetupUnavailable,
  EngagementValidationPage,
} from "./engagement-pages";
import { IssuesPage, RowDetailPage } from "./pages";
import { AppLink, useLocation } from "./router";
import { TourComparisonPage } from "./tour2-pages";
import { WorkspaceHeaderIdentity } from "./object-workspace";
import {
  JourneyRail,
  JourneyStageUnavailable,
  journeyStageForLocation,
} from "./journey-rail";
import { StageHelpPanel } from "./stage-help";
import { SafeRenderBoundary } from "./ui";
import { DestinationPage } from "./destination-workshop";

function safeOrdinal(pathname: string): number | null {
  const match = /^\/rows\/(\d+)$/.exec(pathname);
  if (!match) return null;
  const ordinal = Number(match[1]);
  return Number.isSafeInteger(ordinal) && ordinal > 0 ? ordinal : null;
}

export function EngagementShell({
  context,
  journey,
}: {
  context: EngagementContext;
  journey: Journey;
}) {
  const location = useLocation();
  const rowOrdinal = safeOrdinal(location.pathname);
  const firstIssuesReady =
    context.engagement?.phase === "FIRST_ISSUES_READY" &&
    context.engagement.validationStage === "COMPLETE" &&
    context.engagement.active.validationRunId !== null;
  const setupCurrent =
    location.pathname === "/" || location.pathname === "/setup";
  const activeStage = journeyStageForLocation(journey, location);
  const completedMultiEntityValidation =
    context.engagement?.validationStage === "COMPLETE" &&
    journey.binding.entityCount !== null &&
    journey.binding.entityCount > 1;

  let page;
  if (activeStage?.status === "LOCKED") {
    page = <JourneyStageUnavailable stage={activeStage} />;
  } else if (setupCurrent) {
    page = (
      <EngagementSetupPage context={context} workspace={journey.workspace} />
    );
  } else if (location.pathname === "/destination") {
    page = <DestinationPage />;
  } else if (location.pathname === "/mapping") {
    page = <EngagementMappingContractPage />;
  } else if (location.pathname === "/issues") {
    page = (
      <IssuesPage
        location={location}
        decisionWritesEnabled={false}
        engagementBound
      />
    );
  } else if (location.pathname === "/comparison") {
    page = <TourComparisonPage entityCount={journey.binding.entityCount} />;
  } else if (location.pathname === "/validation") {
    page = <EngagementValidationPage />;
  } else if (rowOrdinal !== null) {
    page = (
      <RowDetailPage
        location={location}
        ordinal={rowOrdinal}
        governedDecisionsEnabled={false}
      />
    );
  } else {
    page = <EngagementSetupUnavailable />;
  }

  return (
    <div className="app-shell engagement-shell">
      <a className="skip-link" href="#main-content">
        Skip to main content
      </a>
      <header className="app-header">
        <div className="brand-block">
          <p>Migration Stream</p>
          <strong>Migration Journey</strong>
        </div>
        <WorkspaceHeaderIdentity status={journey.workspace} />
      </header>
      <aside
        className="read-only-banner"
        aria-label={
          firstIssuesReady
            ? "Multi-entity First Issues authority boundary"
            : completedMultiEntityValidation
              ? "Multi-entity validation authority boundary"
              : "Local setup boundary"
        }
      >
        <strong>
          {firstIssuesReady
            ? "Multi-entity authority stops at First Issues"
            : completedMultiEntityValidation
              ? "Multi-entity authority stops at validation evidence"
              : "Local migration-readiness proof"}
        </strong>
        <span>
          {firstIssuesReady
            ? "Header, item, and relationship findings are read-only here. Governed decisions, corrections, reconciliation, result approval, publication, external transmission, source mutation, mapping mutation, and target-system actions are disabled."
            : completedMultiEntityValidation
              ? "Validation evidence is read-only here. Governed decisions, corrections, reconciliation, result approval, publication, external transmission, source mutation, mapping mutation, and target-system actions are disabled."
              : "No external transmission, target write, publication, or automatic profile approval."}
        </span>
      </aside>
      <JourneyRail journey={journey} location={location} />
      <StageHelpPanel journey={journey} location={location} />
      <main id="main-content" className="main-content" data-layout="responsive">
        {rowOrdinal !== null ? (
          <nav className="breadcrumbs" aria-label="Breadcrumb">
            <AppLink href="/issues">Issues</AppLink>
            <span aria-hidden="true">/</span>
            <span aria-current="page">Row Detail</span>
          </nav>
        ) : null}
        <SafeRenderBoundary key={`${location.pathname}${location.search}`}>
          {page}
        </SafeRenderBoundary>
      </main>
      <footer className="app-footer">
        <p>
          <strong>Bounded local engagement surface.</strong> Server contracts
          remain authority.
        </p>
        <p>
          {context.supportedContracts.length} setup contract versions exposed.
        </p>
      </footer>
    </div>
  );
}
