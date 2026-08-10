import { useCallback, useEffect, useState } from "react";
import {
  ContractError,
  SCHEMAS,
  ENGAGEMENT_COMMAND_SCHEMAS,
  OBJECT_WORKSPACE_COMMAND_SCHEMAS,
  DESTINATION_COMMAND_SCHEMAS,
  decodeApplicationContext,
  decodeContract,
  decodeEngagementCommandResult,
  decodeHttpError,
  decodeRevisedTourCommandResult,
  decodeSourcePreflightReservation,
  type EngagementStateCommand,
  type ReserveSourcePreflightCommand,
  type RevisedTourCommand,
  type StartRevisedTourCommand,
  type ReserveRevisedSourcePreflightCommand,
  type AcceptRevisedSourceCommand,
  type LaunchRevisedValidationCommand,
  type TourChain,
  type AcceptRevisedSourceResult,
  type LaunchRevisedValidationResult,
  type CreateRemediationPackCommand,
  type RemediationPackCommandResult,
  type SchemaName,
  type SourcePreflightDocument,
  type DecisionCommand,
  type DecisionResult,
  type ResultCommand,
  type ResultCommandResult,
  type PublicationRequest,
  type PublicationRecord,
  type ObjectWorkspaceCommand,
  type ObjectWorkspaceRecord,
  type DestinationCommand,
  type ReserveEdmxCommand,
  type CreateManualCatalogueCommand,
  type StartDestinationDraftCommand,
  type SaveDestinationDraftCommand,
  type RebaseDestinationDraftCommand,
  type FreezeDestinationDraftCommand,
  type ApproveDestinationProfileCommand,
  type CarryForwardMappingCommand,
  type DestinationArtifactReservation,
  type DestinationArtifactView,
  type DestinationDraftView,
  type FrozenDestinationProfileView,
  type ApprovedDestinationProfileView,
  type DestinationRebaseView,
  type DestinationCarryForwardView,
  validateDecisionCommand,
  validateEngagementStateCommand,
  validateObjectWorkspaceCommand,
  validateDestinationCommand,
  validateReserveSourcePreflightCommand,
  validateRevisedTourCommand,
  validateCreateRemediationPackCommand,
  validateResultCommand,
  resultCommandResponseSchema,
} from "./contracts";

export type ReadState<T> =
  | { status: "loading" }
  | { status: "success"; data: T }
  | { status: "error"; error: ContractError };

export const AUTHORITY_CHANGED_EVENT = "migration-stream:authority-changed";

interface AuthorityChangedDetail {
  refreshContext: boolean;
}

function signalAuthorityChanged(refreshContext = true): void {
  window.dispatchEvent(
    new CustomEvent<AuthorityChangedDetail>(AUTHORITY_CHANGED_EVENT, {
      detail: { refreshContext },
    }),
  );
}

function ensureSameOriginApi(path: string): void {
  if (!path.startsWith("/api/v1/") || path.startsWith("//")) {
    throw new ContractError(
      "UNSAFE_REQUEST",
      "The workbench refused a request outside its same-origin API.",
    );
  }
}

async function readJson(response: Response): Promise<unknown> {
  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.toLowerCase().includes("application/json")) {
    throw new ContractError(
      "INVALID_CONTENT_TYPE",
      "The server returned an unexpected content type.",
    );
  }
  try {
    return (await response.json()) as unknown;
  } catch {
    throw new ContractError(
      "INVALID_JSON",
      "The server returned malformed JSON.",
    );
  }
}

export async function requestContract<T>(
  path: string,
  schema: SchemaName,
  signal?: AbortSignal,
): Promise<T> {
  ensureSameOriginApi(path);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "GET",
      headers: { Accept: "application/json" },
      credentials: "same-origin",
      cache: "no-store",
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNAVAILABLE",
      "The local read service is unavailable.",
      true,
    );
  }

  const document = await readJson(response);
  if (!response.ok) {
    const envelope = decodeHttpError(document);
    if (envelope) {
      throw new ContractError(
        envelope.error.code,
        envelope.error.message,
        envelope.error.retryable,
        envelope.error.request_id,
      );
    }
    throw new ContractError(
      "HTTP_ERROR",
      `The local read service returned HTTP ${response.status}.`,
      response.status >= 500,
    );
  }
  if (
    schema === SCHEMAS.context ||
    schema === SCHEMAS.governedContext ||
    schema === SCHEMAS.resultApprovalContext ||
    schema === SCHEMAS.publicationContext ||
    schema === SCHEMAS.engagementContext
  ) {
    return decodeApplicationContext(document) as T;
  }
  return decodeContract<T>(document, schema);
}

export async function submitDecisionCommand(
  path: string,
  command: DecisionCommand,
  idempotencyKey: string,
  signal?: AbortSignal,
): Promise<DecisionResult> {
  ensureSameOriginApi(path);
  const validated = validateDecisionCommand(command);
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._:-]{15,127}$/.test(idempotencyKey)) {
    throw new ContractError(
      "INVALID_IDEMPOTENCY_KEY",
      "The decision retry identity is invalid.",
    );
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": idempotencyKey,
        "X-Migration-Stream-Intent": "governed-decision-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The command result is uncertain. Retry with the same decision identity.",
      true,
    );
  }
  const document = await readJson(response);
  if (!response.ok) {
    const envelope = decodeHttpError(document);
    if (envelope) {
      throw new ContractError(
        envelope.error.code,
        envelope.error.message,
        envelope.error.retryable,
        envelope.error.request_id,
      );
    }
    throw new ContractError(
      "DECISION_HTTP_ERROR",
      `The local decision service returned HTTP ${response.status}.`,
      response.status >= 500,
    );
  }
  const result = decodeContract<DecisionResult>(
    document,
    SCHEMAS.decisionCommandResult,
  );
  signalAuthorityChanged();
  return result;
}

function resultCommandPath(command: ResultCommand): string {
  switch (command.schema_version) {
    case "migration-stream/write/calculate-reconciliation/v1":
      return "/api/v1/result-reconciliation/calculate";
    case "migration-stream/write/create-result-candidate/v1":
      return "/api/v1/result-candidates/create";
    case "migration-stream/write/approve-result-candidate/v1":
      return `/api/v1/result-candidates/${encodeURIComponent(command.candidate_id)}/approve`;
    case "migration-stream/write/reject-result-candidate/v1":
      return `/api/v1/result-candidates/${encodeURIComponent(command.candidate_id)}/reject`;
    case "migration-stream/write/revoke-result-approval/v1":
      return `/api/v1/result-candidates/${encodeURIComponent(command.candidate_id)}/revoke`;
    case "migration-stream/write/supersede-result-approval/v1":
      return `/api/v1/result-candidates/${encodeURIComponent(command.candidate_id)}/supersede`;
  }
}

export async function submitResultCommand(
  path: string,
  command: ResultCommand,
  idempotencyKey: string,
  signal?: AbortSignal,
): Promise<ResultCommandResult> {
  ensureSameOriginApi(path);
  const validated = validateResultCommand(command);
  if (path !== resultCommandPath(validated)) {
    throw new ContractError(
      "INVALID_RESULT_ROUTE",
      "The migration-result command does not match its bounded API route.",
    );
  }
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._:-]{15,127}$/.test(idempotencyKey)) {
    throw new ContractError(
      "INVALID_IDEMPOTENCY_KEY",
      "The migration-result retry identity is invalid.",
    );
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": idempotencyKey,
        "X-Migration-Stream-Intent": "migration-result-approval-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The migration-result command outcome is uncertain. Retry the exact command with the same migration-result identity.",
      true,
    );
  }
  const document = await readJson(response);
  if (!response.ok) {
    const envelope = decodeHttpError(document);
    if (envelope) {
      throw new ContractError(
        envelope.error.code,
        envelope.error.message,
        envelope.error.retryable,
        envelope.error.request_id,
      );
    }
    throw new ContractError(
      "RESULT_HTTP_ERROR",
      `The local migration-result service returned HTTP ${response.status}.`,
      response.status >= 500,
    );
  }
  const result = decodeContract<ResultCommandResult>(
    document,
    resultCommandResponseSchema(validated),
  );
  signalAuthorityChanged();
  return result;
}

export async function submitPublicationCommand(
  path: string,
  request: PublicationRequest,
  signal?: AbortSignal,
): Promise<PublicationRecord> {
  ensureSameOriginApi(path);
  if (path !== "/api/v1/publications") {
    throw new ContractError(
      "INVALID_PUBLICATION_ROUTE",
      "The publication command does not match its bounded API route.",
    );
  }
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._:-]{15,127}$/.test(request.idempotency_key)) {
    throw new ContractError(
      "INVALID_IDEMPOTENCY_KEY",
      "The readiness-package retry identity is invalid.",
    );
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": request.idempotency_key,
        "X-Migration-Stream-Intent": "readiness-publication-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(request),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The readiness-package command outcome is uncertain. Retry the unchanged command with the same package identity.",
      true,
    );
  }
  const document = await readJson(response);
  if (!response.ok) {
    const envelope = decodeHttpError(document);
    if (envelope) {
      throw new ContractError(
        envelope.error.code,
        envelope.error.message,
        envelope.error.retryable,
        envelope.error.request_id,
      );
    }
    throw new ContractError(
      "PUBLICATION_HTTP_ERROR",
      `The local publication service returned HTTP ${response.status}.`,
      response.status >= 500,
    );
  }
  const result = decodeContract<PublicationRecord>(
    document,
    SCHEMAS.publicationDetail,
  );
  signalAuthorityChanged();
  return result;
}

function ensureIdempotencyKey(value: string): void {
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{15,127}$/.test(value)) {
    throw new ContractError(
      "INVALID_IDEMPOTENCY_KEY",
      "The local engagement retry identity is invalid.",
    );
  }
}

function engagementStateCommandRoute(command: EngagementStateCommand): string {
  switch (command.schemaVersion) {
    case ENGAGEMENT_COMMAND_SCHEMAS.create:
      return "/api/v1/engagement";
    case ENGAGEMENT_COMMAND_SCHEMAS.acceptSource:
      return `/api/v1/engagement/source-preflights/${encodeURIComponent(command.preflightId)}/accept`;
    case ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack:
      return "/api/v1/engagement/object-packs/select";
    case ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile:
      return "/api/v1/engagement/object-packs/approve";
    case ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping:
      return "/api/v1/engagement/mappings/prepare";
    case ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft:
      return "/api/v1/engagement/mapping-drafts/save";
    case ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft:
      return "/api/v1/engagement/mapping-drafts/submit";
    case ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion:
      return "/api/v1/engagement/mappings/approve";
    case ENGAGEMENT_COMMAND_SCHEMAS.launchValidation:
      return "/api/v1/engagement/validation/launch";
  }
}

function engagementIntent(command: EngagementStateCommand): string {
  switch (command.schemaVersion) {
    case ENGAGEMENT_COMMAND_SCHEMAS.create:
      return "engagement-create-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.acceptSource:
      return "engagement-source-accept-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.selectObjectPack:
      return "engagement-object-pack-select-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.approveObjectPackProfile:
      return "engagement-profile-approval-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.prepareMapping:
      return "engagement-mapping-prepare-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.saveMappingDraft:
      return "engagement-mapping-draft-save-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.submitMappingDraft:
      return "engagement-mapping-draft-submit-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.approveMappingVersion:
      return "engagement-mapping-version-approve-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.launchValidation:
      return "engagement-validation-launch-v1";
  }
}

async function commandResponse(
  response: Response,
  serviceName: string,
): Promise<unknown> {
  const document = await readJson(response);
  if (!response.ok) {
    const envelope = decodeHttpError(document);
    if (envelope) {
      throw new ContractError(
        envelope.error.code,
        envelope.error.message,
        envelope.error.retryable,
        envelope.error.request_id,
      );
    }
    throw new ContractError(
      "ENGAGEMENT_HTTP_ERROR",
      `The local ${serviceName} returned HTTP ${response.status}.`,
      response.status >= 500,
    );
  }
  return document;
}

function objectWorkspaceCommandRoute(command: ObjectWorkspaceCommand): string {
  return command.schemaVersion === OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize
    ? "/api/v1/object-workspace/initialize"
    : "/api/v1/object-workspace/adopt";
}

function objectWorkspaceIntent(command: ObjectWorkspaceCommand): string {
  return command.schemaVersion === OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize
    ? "object-workspace-initialize-v1"
    : "object-workspace-adopt-v1";
}

export async function submitObjectWorkspaceCommand(
  command: ObjectWorkspaceCommand,
  signal?: AbortSignal,
): Promise<ObjectWorkspaceRecord> {
  const validated = validateObjectWorkspaceCommand(command);
  const path = objectWorkspaceCommandRoute(validated);
  ensureSameOriginApi(path);
  ensureIdempotencyKey(validated.idempotencyKey);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": validated.idempotencyKey,
        "X-Migration-Stream-Intent": objectWorkspaceIntent(validated),
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The workspace outcome is uncertain. Resolve the durable workspace state before retrying with the unchanged retry identity.",
      true,
    );
  }
  const document = await commandResponse(response, "object workspace service");
  const result = decodeContract<ObjectWorkspaceRecord>(
    document,
    SCHEMAS.objectWorkspace,
  );
  signalAuthorityChanged();
  return result;
}

function destinationCommandRoute(command: DestinationCommand): string {
  switch (command.schemaVersion) {
    case DESTINATION_COMMAND_SCHEMAS.reserveEdmx:
      return "/api/v1/engagement/destination/edmx-imports";
    case DESTINATION_COMMAND_SCHEMAS.createManualCatalogue:
      return "/api/v1/engagement/destination/manual-catalogues";
    case DESTINATION_COMMAND_SCHEMAS.startDraft:
      return "/api/v1/engagement/destination/drafts/start";
    case DESTINATION_COMMAND_SCHEMAS.saveDraft:
      return "/api/v1/engagement/destination/drafts/save";
    case DESTINATION_COMMAND_SCHEMAS.rebaseDraft:
      return "/api/v1/engagement/destination/drafts/rebase";
    case DESTINATION_COMMAND_SCHEMAS.freezeDraft:
      return "/api/v1/engagement/destination/drafts/freeze";
    case DESTINATION_COMMAND_SCHEMAS.approveProfile:
      return "/api/v1/engagement/destination/profiles/approve";
    case DESTINATION_COMMAND_SCHEMAS.carryForwardMapping:
      return "/api/v1/engagement/destination/mappings/carry-forward";
  }
}

function destinationCommandIntent(command: DestinationCommand): string {
  switch (command.schemaVersion) {
    case DESTINATION_COMMAND_SCHEMAS.reserveEdmx:
      return "engagement-destination-edmx-reserve-v1";
    case DESTINATION_COMMAND_SCHEMAS.createManualCatalogue:
      return "engagement-destination-manual-catalogue-v1";
    case DESTINATION_COMMAND_SCHEMAS.startDraft:
      return "engagement-destination-draft-start-v1";
    case DESTINATION_COMMAND_SCHEMAS.saveDraft:
      return "engagement-destination-draft-save-v1";
    case DESTINATION_COMMAND_SCHEMAS.rebaseDraft:
      return "engagement-destination-draft-rebase-v1";
    case DESTINATION_COMMAND_SCHEMAS.freezeDraft:
      return "engagement-destination-draft-freeze-v1";
    case DESTINATION_COMMAND_SCHEMAS.approveProfile:
      return "engagement-destination-profile-approve-v1";
    case DESTINATION_COMMAND_SCHEMAS.carryForwardMapping:
      return "engagement-destination-mapping-carry-forward-v1";
  }
}

function destinationCommandSchema(command: DestinationCommand): SchemaName {
  switch (command.schemaVersion) {
    case DESTINATION_COMMAND_SCHEMAS.reserveEdmx:
      return SCHEMAS.destinationArtifactReservation;
    case DESTINATION_COMMAND_SCHEMAS.createManualCatalogue:
      return SCHEMAS.destinationArtifact;
    case DESTINATION_COMMAND_SCHEMAS.startDraft:
    case DESTINATION_COMMAND_SCHEMAS.saveDraft:
      return SCHEMAS.destinationDraft;
    case DESTINATION_COMMAND_SCHEMAS.rebaseDraft:
      return SCHEMAS.destinationRebase;
    case DESTINATION_COMMAND_SCHEMAS.freezeDraft:
      return SCHEMAS.destinationReviewedProfile;
    case DESTINATION_COMMAND_SCHEMAS.approveProfile:
      return SCHEMAS.destinationProfileApproval;
    case DESTINATION_COMMAND_SCHEMAS.carryForwardMapping:
      return SCHEMAS.destinationCarryForward;
  }
}

export function submitDestinationCommand(
  command: ReserveEdmxCommand,
  signal?: AbortSignal,
): Promise<DestinationArtifactReservation>;
export function submitDestinationCommand(
  command: CreateManualCatalogueCommand,
  signal?: AbortSignal,
): Promise<DestinationArtifactView>;
export function submitDestinationCommand(
  command: StartDestinationDraftCommand,
  signal?: AbortSignal,
): Promise<DestinationDraftView>;
export function submitDestinationCommand(
  command: SaveDestinationDraftCommand,
  signal?: AbortSignal,
): Promise<DestinationDraftView>;
export function submitDestinationCommand(
  command: RebaseDestinationDraftCommand,
  signal?: AbortSignal,
): Promise<DestinationRebaseView>;
export function submitDestinationCommand(
  command: FreezeDestinationDraftCommand,
  signal?: AbortSignal,
): Promise<FrozenDestinationProfileView>;
export function submitDestinationCommand(
  command: ApproveDestinationProfileCommand,
  signal?: AbortSignal,
): Promise<ApprovedDestinationProfileView>;
export function submitDestinationCommand(
  command: CarryForwardMappingCommand,
  signal?: AbortSignal,
): Promise<DestinationCarryForwardView>;
export async function submitDestinationCommand(
  command: DestinationCommand,
  signal?: AbortSignal,
): Promise<
  | DestinationArtifactReservation
  | DestinationArtifactView
  | DestinationDraftView
  | DestinationRebaseView
  | FrozenDestinationProfileView
  | ApprovedDestinationProfileView
  | DestinationCarryForwardView
> {
  const validated = validateDestinationCommand(command);
  ensureIdempotencyKey(validated.idempotencyKey);
  const path = destinationCommandRoute(validated);
  ensureSameOriginApi(path);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": validated.idempotencyKey,
        "X-Migration-Stream-Intent": destinationCommandIntent(validated),
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The destination command outcome is uncertain. Refresh the durable destination authority before retrying with the unchanged identity.",
      true,
    );
  }
  const document = await commandResponse(
    response,
    "destination contract service",
  );
  const result = decodeContract<
    | DestinationArtifactReservation
    | DestinationArtifactView
    | DestinationDraftView
    | DestinationRebaseView
    | FrozenDestinationProfileView
    | ApprovedDestinationProfileView
    | DestinationCarryForwardView
  >(document, destinationCommandSchema(validated));
  signalAuthorityChanged();
  return result;
}

export async function uploadDestinationArtifact(
  attemptId: string,
  file: File,
  idempotencyKey: string,
  signal?: AbortSignal,
): Promise<DestinationArtifactView> {
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$/.test(attemptId)) {
    throw new ContractError(
      "INVALID_DESTINATION_IMPORT_ID",
      "The destination import identity is invalid.",
    );
  }
  ensureIdempotencyKey(idempotencyKey);
  const path = `/api/v1/engagement/destination/edmx-imports/${encodeURIComponent(attemptId)}/content`;
  ensureSameOriginApi(path);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "PUT",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/xml",
        "Idempotency-Key": idempotencyKey,
        "X-Migration-Stream-Intent": "engagement-destination-edmx-content-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: file,
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The EDMX upload outcome is uncertain. Refresh destination evidence before selecting the same file again.",
      true,
    );
  }
  const document = await commandResponse(
    response,
    "destination artifact service",
  );
  const result = decodeContract<DestinationArtifactView>(
    document,
    SCHEMAS.destinationArtifact,
  );
  signalAuthorityChanged();
  return result;
}

export async function submitEngagementStateCommand(
  path: string,
  command: EngagementStateCommand,
  signal?: AbortSignal,
): Promise<unknown> {
  ensureSameOriginApi(path);
  const validated = validateEngagementStateCommand(command);
  const expectedPath = engagementStateCommandRoute(validated);
  if (path !== expectedPath) {
    throw new ContractError(
      "INVALID_ENGAGEMENT_ROUTE",
      "The local engagement command does not match its bounded API route.",
    );
  }
  ensureIdempotencyKey(validated.idempotencyKey);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": validated.idempotencyKey,
        "X-Migration-Stream-Intent": engagementIntent(validated),
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The local command outcome is uncertain. The unchanged retry identity must be resolved against engagement state before retrying.",
      true,
    );
  }
  const document = await commandResponse(response, "engagement service");
  const result = decodeEngagementCommandResult(validated, document);
  signalAuthorityChanged(
    validated.schemaVersion !== ENGAGEMENT_COMMAND_SCHEMAS.launchValidation,
  );
  return result;
}

export async function reserveSourcePreflight(
  command: ReserveSourcePreflightCommand,
  signal?: AbortSignal,
): Promise<SourcePreflightDocument> {
  const path = "/api/v1/engagement/source-preflights";
  ensureSameOriginApi(path);
  const validated = validateReserveSourcePreflightCommand(command);
  ensureIdempotencyKey(validated.idempotencyKey);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": validated.idempotencyKey,
        "X-Migration-Stream-Intent": "engagement-source-preflight-reserve-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The source reservation outcome is uncertain. Reread engagement state before retrying with the same identity.",
      true,
    );
  }
  const document = await commandResponse(
    response,
    "source reservation service",
  );
  const result = decodeSourcePreflightReservation(document);
  signalAuthorityChanged();
  return result;
}

function revisedTourRoute(command: RevisedTourCommand): string {
  switch (command.schemaVersion) {
    case ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour:
      return "/api/v1/engagement/tours/start";
    case ENGAGEMENT_COMMAND_SCHEMAS.reserveRevisedSourcePreflight:
      return "/api/v1/engagement/tours/revised-source-preflights";
    case ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource:
      return `/api/v1/engagement/tours/revised-source-preflights/${encodeURIComponent(command.preflightId)}/accept`;
    case ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation:
      return "/api/v1/engagement/tours/validation/launch";
  }
}

function revisedTourIntent(command: RevisedTourCommand): string {
  switch (command.schemaVersion) {
    case ENGAGEMENT_COMMAND_SCHEMAS.startRevisedTour:
      return "engagement-revised-tour-start-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.reserveRevisedSourcePreflight:
      return "engagement-revised-source-reserve-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.acceptRevisedSource:
      return "engagement-revised-source-accept-v1";
    case ENGAGEMENT_COMMAND_SCHEMAS.launchRevisedValidation:
      return "engagement-revised-validation-launch-v1";
  }
}

export function submitRevisedTourCommand(
  command: StartRevisedTourCommand,
  signal?: AbortSignal,
): Promise<TourChain>;
export function submitRevisedTourCommand(
  command: ReserveRevisedSourcePreflightCommand,
  signal?: AbortSignal,
): Promise<SourcePreflightDocument>;
export function submitRevisedTourCommand(
  command: AcceptRevisedSourceCommand,
  signal?: AbortSignal,
): Promise<AcceptRevisedSourceResult>;
export function submitRevisedTourCommand(
  command: LaunchRevisedValidationCommand,
  signal?: AbortSignal,
): Promise<LaunchRevisedValidationResult>;
export async function submitRevisedTourCommand(
  command: RevisedTourCommand,
  signal?: AbortSignal,
): Promise<
  | TourChain
  | SourcePreflightDocument
  | AcceptRevisedSourceResult
  | LaunchRevisedValidationResult
> {
  const validated = validateRevisedTourCommand(command);
  ensureIdempotencyKey(validated.idempotencyKey);
  const path = revisedTourRoute(validated);
  ensureSameOriginApi(path);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": validated.idempotencyKey,
        "X-Migration-Stream-Intent": revisedTourIntent(validated),
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The revised-validation command outcome is uncertain. Refresh the immutable source-revision evidence before retrying the unchanged command identity.",
      true,
    );
  }
  const document = await commandResponse(
    response,
    "revised-validation service",
  );
  const result = decodeRevisedTourCommandResult(validated, document);
  signalAuthorityChanged();
  return result;
}

export async function submitRemediationPackCommand(
  command: CreateRemediationPackCommand,
  signal?: AbortSignal,
): Promise<RemediationPackCommandResult> {
  const validated = validateCreateRemediationPackCommand(command);
  const path = "/api/v1/engagement/remediation-packs";
  ensureSameOriginApi(path);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": validated.idempotencyKey,
        "X-Migration-Stream-Intent": "engagement-remediation-pack-create-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: JSON.stringify(validated),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw error;
    }
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The remediation-pack outcome is uncertain. Refresh local pack history before retrying the unchanged command identity.",
      true,
    );
  }
  const document = await commandResponse(response, "remediation-pack service");
  return decodeContract<RemediationPackCommandResult>(
    document,
    SCHEMAS.remediationPack,
  );
}

export async function uploadSourcePreflight(
  preflightId: string,
  file: File,
  idempotencyKey: string,
  signal?: AbortSignal,
): Promise<SourcePreflightDocument["preflight"]> {
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$/.test(preflightId)) {
    throw new ContractError(
      "INVALID_PREFLIGHT_ID",
      "The source reservation identity is invalid.",
    );
  }
  ensureIdempotencyKey(idempotencyKey);
  const path = `/api/v1/engagement/source-preflights/${encodeURIComponent(preflightId)}/content`;
  ensureSameOriginApi(path);
  let response: Response;
  try {
    response = await fetch(path, {
      method: "PUT",
      mode: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/zip",
        "Idempotency-Key": idempotencyKey,
        "X-Migration-Stream-Intent": "engagement-source-preflight-content-v1",
      },
      credentials: "same-origin",
      cache: "no-store",
      body: file,
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError")
      throw error;
    throw new ContractError(
      "NETWORK_UNCERTAIN",
      "The source upload outcome is uncertain. Choose the ZIP again after the engagement state has been refreshed.",
      true,
    );
  }
  const document = await commandResponse(response, "source preflight service");
  const result = decodeContract<SourcePreflightDocument["preflight"]>(
    document,
    SCHEMAS.sourcePreflight,
  );
  signalAuthorityChanged();
  return result;
}

export function useRead<T>(
  path: string,
  schema: SchemaName,
): {
  state: ReadState<T>;
  retry: () => void;
} {
  const [attempt, setAttempt] = useState(0);
  const requestKey = `${schema}\u0000${path}\u0000${attempt}`;
  const [result, setResult] = useState<{
    key: string;
    state: ReadState<T>;
  }>({ key: requestKey, state: { status: "loading" } });

  useEffect(() => {
    const controller = new AbortController();
    void requestContract<T>(path, schema, controller.signal).then(
      (data) =>
        setResult({ key: requestKey, state: { status: "success", data } }),
      (error: unknown) => {
        if (error instanceof DOMException && error.name === "AbortError")
          return;
        const safeError =
          error instanceof ContractError
            ? error
            : new ContractError(
                "UNEXPECTED_CLIENT_ERROR",
                "The response could not be displayed safely.",
              );
        setResult({
          key: requestKey,
          state: { status: "error", error: safeError },
        });
      },
    );
    return () => controller.abort();
  }, [path, requestKey, schema]);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  const visibleState =
    result.key === requestKey
      ? result.state
      : result.state.status === "success"
        ? result.state
        : { status: "loading" as const };
  return {
    state: visibleState,
    retry,
  };
}

export function apiQuery(
  route: string,
  values: Record<string, string | number | boolean | null | undefined>,
): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) {
    if (value !== null && value !== undefined && value !== "") {
      query.set(key, String(value));
    }
  }
  const suffix = query.toString();
  return suffix ? `${route}?${suffix}` : route;
}
