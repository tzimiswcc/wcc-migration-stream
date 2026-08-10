import { useState } from "react";
import type {
  ContractError,
  CorrectionOperation,
  DecisionResult,
} from "./contracts";

export const DECISION_ACTIONS = [
  "correct",
  "waive",
  "accept-as-is",
  "exclude-row",
] as const;

export type DecisionAction = (typeof DECISION_ACTIONS)[number];

export interface DecisionDraft {
  practitionerIdentifier: string;
  reason: string;
  evidenceReference: string;
  operation: CorrectionOperation;
  proposedValue: string;
}

export type DecisionFlowState =
  | {
      status: "editing";
      action: DecisionAction;
      idempotencyKey: string;
      draft: DecisionDraft;
    }
  | {
      status: "confirming";
      action: DecisionAction;
      idempotencyKey: string;
      draft: DecisionDraft;
    }
  | {
      status: "submitting";
      action: DecisionAction;
      idempotencyKey: string;
      draft: DecisionDraft;
    }
  | {
      status: "result";
      action: DecisionAction;
      idempotencyKey: string;
      draft: DecisionDraft;
      result: DecisionResult;
    }
  | {
      status: "error";
      action: DecisionAction;
      idempotencyKey: string;
      draft: DecisionDraft;
      error: ContractError;
    };

const EMPTY_DRAFT: DecisionDraft = {
  practitionerIdentifier: "",
  reason: "",
  evidenceReference: "",
  operation: "REPLACE_EFFECTIVE_TEXT",
  proposedValue: "",
};

function newIdempotencyKey(): string {
  return `decision-${globalThis.crypto.randomUUID()}`;
}

export function parseDecisionAction(value: string): DecisionAction | null {
  return DECISION_ACTIONS.includes(value as DecisionAction)
    ? (value as DecisionAction)
    : null;
}

export function useDecisionFlow(action: DecisionAction) {
  const [state, setState] = useState<DecisionFlowState>(() => ({
    status: "editing",
    action,
    idempotencyKey: newIdempotencyKey(),
    draft: EMPTY_DRAFT,
  }));

  return {
    state,
    review(draft: DecisionDraft) {
      setState((current) => ({ ...current, status: "confirming", draft }));
    },
    edit() {
      setState((current) =>
        current.status === "error"
          ? {
              status: "editing",
              action: current.action,
              idempotencyKey: newIdempotencyKey(),
              draft: current.draft,
            }
          : { ...current, status: "editing" },
      );
    },
    submit() {
      setState((current) => ({ ...current, status: "submitting" }));
    },
    succeed(result: DecisionResult) {
      setState((current) => ({ ...current, status: "result", result }));
    },
    fail(error: ContractError) {
      setState((current) => ({ ...current, status: "error", error }));
    },
    retry() {
      setState((current) =>
        current.status === "error" && current.error.retryable
          ? { ...current, status: "confirming" }
          : current,
      );
    },
  };
}
