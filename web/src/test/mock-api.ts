import { vi } from "vitest";
import {
  correctionFixture,
  decisionCommandResultFixture,
  decisionHistoryFixture,
  decisionIssueFixture,
  decisionResultFixture,
  profilingSamplesFixture,
  journeyFixture,
  routeFixtures,
  rowFixture,
} from "./fixtures";
import { SCHEMAS } from "../contracts";

export interface CapturedRequest {
  url: URL;
  method: string;
  headers: Headers;
  body: string | null;
  rawBody: BodyInit | null;
}

type OverrideFactory = (url: URL, request: CapturedRequest) => unknown;

function isOverrideFactory(value: unknown): value is OverrideFactory {
  return typeof value === "function";
}

function jsonResponse(document: unknown, status = 200): Response {
  return new Response(JSON.stringify(document), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function fixtureJourneyForContext(value: unknown): unknown {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return journeyFixture;
  }
  const context = value as Record<string, unknown>;
  const schema = context.schema_version ?? context.schemaVersion;
  if (
    schema === SCHEMAS.governedContext ||
    schema === SCHEMAS.resultApprovalContext ||
    schema === SCHEMAS.publicationContext
  ) {
    const current =
      schema === SCHEMAS.publicationContext
        ? "PACKAGE"
        : schema === SCHEMAS.resultApprovalContext
          ? "RECONCILIATION"
          : "DECISIONS";
    return {
      ...journeyFixture,
      currentStageId: current,
      stages: journeyFixture.stages.map((stage) => {
        if (stage.id === current) {
          return {
            ...stage,
            status: "CURRENT",
            available: true,
            href:
              stage.id === "DECISIONS"
                ? "/decisions"
                : stage.id === "RECONCILIATION"
                  ? "/reconciliation"
                  : "/publication",
            unavailableReason: null,
          };
        }
        if (stage.id === "DECISIONS") {
          return {
            ...stage,
            status:
              current === "RECONCILIATION" || current === "PACKAGE"
                ? "COMPLETED"
                : "AVAILABLE",
            available: true,
            href: "/decisions",
            unavailableReason: null,
          };
        }
        if (stage.id === "RECONCILIATION") {
          return {
            ...stage,
            status: current === "PACKAGE" ? "COMPLETED" : "AVAILABLE",
            available: true,
            href: "/reconciliation",
            unavailableReason: null,
          };
        }
        if (stage.id === "PACKAGE") {
          return current === "PACKAGE"
            ? stage
            : {
                ...stage,
                status: "LOCKED",
                available: false,
                href: null,
                unavailableReason:
                  "A readiness package requires eligible approved local result evidence.",
              };
        }
        if (stage.id === "ISSUES") {
          return { ...stage, status: "COMPLETED" };
        }
        return stage.status === "CURRENT"
          ? { ...stage, status: "COMPLETED" }
          : stage;
      }),
    };
  }
  if (schema === SCHEMAS.engagementContext) {
    const engagement =
      typeof context.engagement === "object" &&
      context.engagement !== null &&
      !Array.isArray(context.engagement)
        ? (context.engagement as Record<string, unknown>)
        : null;
    if (engagement?.validationStage === "COMPLETE") {
      const active =
        typeof engagement.active === "object" &&
        engagement.active !== null &&
        !Array.isArray(engagement.active)
          ? (engagement.active as Record<string, unknown>)
          : {};
      const sales =
        active.objectPackId === "synthetic.sales-document.readiness";
      return {
        ...journeyFixture,
        currentStageId: "ISSUES",
        binding: {
          ...journeyFixture.binding,
          engagementId:
            typeof engagement.engagementId === "string"
              ? engagement.engagementId
              : null,
          engagementGeneration:
            typeof engagement.generation === "number"
              ? engagement.generation
              : null,
          jobId: typeof active.jobId === "string" ? active.jobId : null,
          validationRunId:
            typeof active.validationRunId === "string"
              ? active.validationRunId
              : null,
          entityCount: sales ? 2 : 1,
        },
        stages: journeyFixture.stages.map((stage) => {
          if (stage.id === "COMPARE") {
            return sales
              ? {
                  ...stage,
                  status: "AVAILABLE",
                  available: true,
                  href: "/comparison",
                  unavailableReason: null,
                }
              : stage;
          }
          if (stage.id === "ISSUES") {
            return {
              ...stage,
              status: "CURRENT",
              available: true,
              href: "/issues",
              unavailableReason: null,
            };
          }
          if (
            sales &&
            ["DECISIONS", "RECONCILIATION", "PACKAGE"].includes(stage.id)
          ) {
            return {
              ...stage,
              status: "LOCKED",
              available: false,
              href: null,
              unavailableReason:
                "Multi-entity downstream authority is not implemented yet.",
            };
          }
          return stage.status === "CURRENT"
            ? { ...stage, status: "COMPLETED" }
            : stage;
        }),
      };
    }
  }
  return journeyFixture;
}

export function mockApi(overrides: Record<string, unknown> = {}) {
  const calls: URL[] = [];
  const requests: CapturedRequest[] = [];
  const mock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const raw = input instanceof Request ? input.url : String(input);
      const url = new URL(raw, window.location.origin);
      const method = (
        init?.method ?? (input instanceof Request ? input.method : "GET")
      ).toUpperCase();
      const headers = new Headers(
        input instanceof Request ? input.headers : undefined,
      );
      new Headers(init?.headers).forEach((value, key) =>
        headers.set(key, value),
      );
      const rawBody = init?.body ?? null;
      const body =
        typeof init?.body === "string"
          ? init.body
          : input instanceof Request && method !== "GET"
            ? await input.clone().text()
            : null;
      const request = { url, method, headers, body, rawBody };
      calls.push(url);
      requests.push(request);
      let value: unknown =
        overrides[`${method} ${url.pathname}`] ?? overrides[url.pathname];
      if (
        value === undefined &&
        method === "GET" &&
        url.pathname === "/api/v1/journey"
      ) {
        const contextOverride =
          overrides["GET /api/v1/context"] ??
          overrides["/api/v1/context"] ??
          routeFixtures["/api/v1/context"];
        value = isOverrideFactory(contextOverride)
          ? journeyFixture
          : fixtureJourneyForContext(contextOverride);
      }
      if (value === undefined) {
        if (
          method === "POST" &&
          /^\/api\/v1\/issues\/[^/]+\/decisions\/(correct|waive|accept-as-is|exclude-row)$/.test(
            url.pathname,
          )
        ) {
          value = decisionCommandResultFixture;
        } else if (
          method === "GET" &&
          /^\/api\/v1\/issues\/[^/]+\/decision-detail$/.test(url.pathname)
        ) {
          value = decisionIssueFixture;
        } else if (
          method === "GET" &&
          /^\/api\/v1\/issues\/[^/]+\/decisions$/.test(url.pathname)
        ) {
          value = decisionHistoryFixture;
        } else if (
          method === "GET" &&
          /^\/api\/v1\/corrections\/[^/]+$/.test(url.pathname)
        ) {
          value = correctionFixture;
        } else if (
          method === "GET" &&
          /^\/api\/v1\/decisions\/[^/]+$/.test(url.pathname)
        ) {
          value = decisionResultFixture;
        } else if (url.pathname.startsWith("/api/v1/rows/")) value = rowFixture;
        else if (
          url.pathname === "/api/v1/profiling" &&
          url.searchParams.get("include_samples") === "true"
        ) {
          value = profilingSamplesFixture;
        } else value = routeFixtures[url.pathname];
      }
      if (isOverrideFactory(value)) value = await value(url, request);
      if (value instanceof Response) return value;
      if (value === undefined) {
        return jsonResponse(
          {
            schema_version: "migration-stream/http-error/v1",
            ok: false,
            error: {
              code: "NOT_FOUND",
              message: "No synthetic route fixture exists.",
              request_id: "request-not-found",
              retryable: false,
            },
          },
          404,
        );
      }
      return jsonResponse(value);
    },
  );
  vi.stubGlobal("fetch", mock);
  return { mock, calls, requests };
}

export function errorResponse(document: unknown, status = 409): Response {
  return jsonResponse(document, status);
}
