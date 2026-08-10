import { useId, useState } from "react";
import type { Journey, JourneyStageId } from "./contracts";
import { journeyStageForLocation } from "./journey-rail";

type StageHelp = {
  title: string;
  purpose: string;
  next: string;
  boundary: string;
};

const STAGE_HELP: Record<JourneyStageId, StageHelp> = {
  SETUP: {
    title: "Set up one local workspace",
    purpose:
      "Name and attribute the startup-selected local workspace, then create or reopen its engagement. The browser does not choose a server path.",
    next: "Establish the workspace, then begin with either Destination or Source when the journey makes it available.",
    boundary:
      "This does not approve a profile, accept a source, create a mapping, validate data, or enable an external action.",
  },
  DESTINATION: {
    title: "Review the destination contract",
    purpose:
      "Choose a controlled profile or draft a workspace-local destination from offline metadata or a manual definition. Activate only the fields and relationships needed for this object.",
    next: "Freeze the reviewed draft and separately approve its exact hash before using it for mapping or validation.",
    boundary:
      "Imported metadata is technical evidence only. It is not business authority or a compatibility statement about an external target.",
  },
  SOURCE: {
    title: "Accept and profile the source",
    purpose:
      "Inspect a bounded ZIP/CSV package, explicitly accept it, and review its structural, sample-free profile evidence.",
    next: "Use the accepted source structure together with an approved destination profile in Mapping.",
    boundary:
      "The original accepted package remains immutable. Routine pages do not show received source values.",
  },
  MAPPING: {
    title: "Build the mapping contract",
    purpose:
      "Route source files, bind fields, confirm relationships, define supported operations and governed exact reference tables, then inspect value-free coverage.",
    next: "Save the draft, freeze its exact bytes, and separately approve the mapping before validation.",
    boundary:
      "Mappings use bounded operations and exact reference tables. They do not run arbitrary code, fan out rows, or write to another system.",
  },
  VALIDATE: {
    title: "Validate the approved contract",
    purpose:
      "Run the approved profile and mapping against the accepted local source to create immutable validation evidence and value-suppressed findings.",
    next: "Use Issues to inspect findings, export a governed remediation pack when appropriate, or start a source revision after completion.",
    boundary:
      "Validation is local readiness evidence. It does not modify the source or confirm an external target outcome.",
  },
  COMPARE: {
    title: "Compare source revisions",
    purpose:
      "Keep earlier runs immutable while comparing adjacent accepted revisions as CLOSED, REMAINING, and NEW findings.",
    next: "If the structure remains compatible, reuse is replayed exactly. Structural drift requires a new governed mapping draft.",
    boundary:
      "Comparison is value-suppressed and does not assert row lineage when it must use the documented full-row multiset fallback.",
  },
  ISSUES: {
    title: "Review local issue evidence",
    purpose:
      "Inspect the validation finding set and, where useful, create an immutable local remediation pack with explicitly selected business-key columns and an optional separately acknowledged offending value.",
    next: "For a single-entity run, continue to Decisions when it becomes available. To refresh the source, start a governed source revision.",
    boundary:
      "Multi-entity work stops at read-only First Issues and Compared Validation. No downstream decisions, reconciliation, or package authority is available there.",
  },
  DECISIONS: {
    title: "Record governed local decisions",
    purpose:
      "For an eligible single-entity run, record bounded issue decisions and non-destructive correction overlays against immutable validation evidence.",
    next: "Continue to Reconciliation after the required local decision evidence is complete.",
    boundary:
      "Decisions and overlays do not change the accepted source, original validation truth, approved authority, or an external system.",
  },
  RECONCILIATION: {
    title: "Reconcile local evidence",
    purpose:
      "Review the governed effective context that joins the source, validation, and eligible local decision evidence before recording a local result approval.",
    next: "When eligible, record local result approval and continue to Package.",
    boundary:
      "This is local evidence reconciliation, not a target-system reconciliation or acceptance process.",
  },
  PACKAGE: {
    title: "Inspect a local readiness package",
    purpose:
      "Generate or inspect a deterministic local readiness package from eligible approved local evidence and verify its manifest and digests.",
    next: "Retain the package with its local evidence. Start a new source revision when refreshed source data is available.",
    boundary:
      "Package generation does not transmit data, deploy anything, or write to an external target system.",
  },
};

export function stageHelpForLocation(
  journey: Journey,
  location: Pick<URL, "pathname" | "hash">,
): JourneyStageId {
  return (
    journeyStageForLocation(journey, location)?.id ?? journey.currentStageId
  );
}

export function StageHelpPanel({
  journey,
  location,
}: {
  journey: Journey;
  location: Pick<URL, "pathname" | "hash">;
}) {
  const [open, setOpen] = useState(false);
  const contentId = useId();
  const stageId = stageHelpForLocation(journey, location);
  const help = STAGE_HELP[stageId];

  return (
    <section className="stage-help" aria-label="Help for this stage">
      <div>
        <strong>Help for this stage</strong>
        <span>{help.title}</span>
      </div>
      <button
        type="button"
        aria-expanded={open}
        aria-controls={contentId}
        onClick={() => setOpen((value) => !value)}
      >
        {open ? "Hide help" : "Show help"}
      </button>
      <div id={contentId} className="stage-help__content" hidden={!open}>
        <p>
          <strong>Here:</strong> {help.purpose}
        </p>
        <p>
          <strong>Next:</strong> {help.next}
        </p>
        <p>
          <strong>Boundary:</strong> {help.boundary}
        </p>
      </div>
    </section>
  );
}
