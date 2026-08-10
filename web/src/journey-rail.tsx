import type { Journey, JourneyStage, JourneyStageId } from "./contracts";
import { PageHeader } from "./ui";

type JourneyLocation = Pick<URL, "pathname" | "hash">;

function setupStageForLocation(
  location: JourneyLocation,
): JourneyStageId | null {
  if (location.pathname !== "/" && location.pathname !== "/setup") return null;
  switch (location.hash) {
    case "#profiled-source":
      return "SOURCE";
    case "#object-pack":
      return "DESTINATION";
    case "#mapping":
    case "#mapping-workspace":
      return "MAPPING";
    case "#validation":
      return "VALIDATE";
    default:
      return "SETUP";
  }
}

function isStageRoute(
  stageId: JourneyStageId,
  location: JourneyLocation,
): boolean {
  const setupStage = setupStageForLocation(location);
  if (setupStage !== null) return stageId === setupStage;
  const { pathname } = location;
  switch (stageId) {
    case "SETUP":
      return false;
    case "DESTINATION":
      return pathname === "/destination" || pathname === "/profile";
    case "SOURCE":
      return pathname === "/files" || pathname === "/profiling";
    case "MAPPING":
      return pathname === "/mapping";
    case "VALIDATE":
      return pathname === "/validation";
    case "COMPARE":
      return pathname === "/comparison";
    case "ISSUES":
      return pathname === "/issues" || pathname.startsWith("/rows/");
    case "DECISIONS":
      return (
        pathname === "/decisions" ||
        pathname.startsWith("/decisions/") ||
        pathname.startsWith("/corrections/") ||
        /^\/issues\/[^/]+/.test(pathname)
      );
    case "RECONCILIATION":
      return pathname === "/reconciliation" || pathname === "/result-approval";
    case "PACKAGE":
      return (
        pathname === "/publication" || pathname.startsWith("/publication/")
      );
  }
}

export function journeyStageForLocation(
  journey: Journey,
  location: JourneyLocation,
): JourneyStage | null {
  return (
    journey.stages.find((stage) => isStageRoute(stage.id, location)) ?? null
  );
}

function stageStatus(status: JourneyStage["status"]): string {
  switch (status) {
    case "COMPLETED":
      return "Complete";
    case "CURRENT":
      return "Current";
    case "AVAILABLE":
      return "Available";
    case "LOCKED":
      return "Locked";
  }
}

export function JourneyRail({
  journey,
  location,
}: {
  journey: Journey;
  location: JourneyLocation;
}) {
  const revision = journey.binding.activeTourNumber;
  const validationRunId = journey.binding.validationRunId;
  return (
    <nav className="journey-rail" aria-label="Migration journey">
      <div className="journey-rail__heading">
        <strong>Migration journey</strong>
        <span>
          {revision === null ? "Current engagement" : `Revision ${revision}`}
          {validationRunId === null ? "" : ` · Run ${validationRunId}`}
        </span>
      </div>
      <ol>
        {journey.stages.map((stage) => {
          const viewed = isStageRoute(stage.id, location);
          return (
            <li
              key={stage.id}
              data-stage={stage.id}
              data-status={stage.status}
              className={`journey-stage journey-stage--${stage.status.toLowerCase()}`}
            >
              {stage.available && stage.href !== null ? (
                <a
                  href={stage.href}
                  aria-label={stage.label}
                  aria-current={viewed ? "page" : undefined}
                >
                  <span>{stage.label}</span>
                  <small>{stageStatus(stage.status)}</small>
                </a>
              ) : (
                <div
                  aria-disabled="true"
                  aria-describedby={`journey-${stage.id.toLowerCase()}-reason`}
                >
                  <span>{stage.label}</span>
                  <small>{stageStatus(stage.status)}</small>
                  <p id={`journey-${stage.id.toLowerCase()}-reason`}>
                    {stage.unavailableReason}
                  </p>
                </div>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

export function JourneyStageUnavailable({ stage }: { stage: JourneyStage }) {
  return (
    <>
      <PageHeader
        eyebrow="Migration journey"
        title={`${stage.label} is locked`}
        description={stage.unavailableReason ?? "This stage is unavailable."}
      />
      <div className="state-panel" role="status">
        <div>
          <strong>Current authority stays unchanged</strong>
          <p>
            Return through an available journey stage. No unavailable route
            grants or changes product authority.
          </p>
        </div>
      </div>
    </>
  );
}
