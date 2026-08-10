import { useRef, useState, type FormEvent, type ReactNode } from "react";
import { submitObjectWorkspaceCommand } from "./api";
import {
  OBJECT_WORKSPACE_COMMAND_SCHEMAS,
  ContractError,
  type ObjectWorkspaceAction,
  type ObjectWorkspaceCommand,
  type ObjectWorkspaceRecord,
  type ObjectWorkspaceStatus,
} from "./contracts";
import { Card, DefinitionGrid, Hash, Value } from "./ui";

function idempotencyKey(scope: string): string {
  const suffix =
    typeof crypto.randomUUID === "function"
      ? crypto.randomUUID().replaceAll("-", "")
      : `${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;
  return `${scope}:${suffix}`.slice(0, 128);
}

function formText(form: FormData, name: string): string {
  const value = form.get(name);
  return typeof value === "string" ? value.trim() : "";
}

function safeError(error: unknown): ContractError {
  return error instanceof ContractError
    ? error
    : new ContractError(
        "UNEXPECTED_CLIENT_ERROR",
        "The local object workspace could not be established safely.",
      );
}

function actionCopy(action: ObjectWorkspaceAction) {
  return action === "INITIALIZE"
    ? {
        title: "Initialize this object workspace",
        description:
          "Give this fresh startup-selected folder a durable local identity before opening an engagement.",
        button: "Initialize object workspace",
        busy: "Initializing workspace…",
      }
    : {
        title: "Adopt this existing object workspace",
        description:
          "Add a durable identity to the existing local Migration Stream evidence before reopening its engagement.",
        button: "Adopt existing workspace",
        busy: "Adopting workspace…",
      };
}

function resumeActionCopy(action: ObjectWorkspaceAction) {
  return action === "INITIALIZE"
    ? {
        title: "Resume object workspace initialization",
        description:
          "The server recovered the exact interrupted initialization command. Review its read-only identity and resume it without creating a new command.",
        button: "Resume workspace initialization",
        busy: "Resuming initialization…",
      }
    : {
        title: "Resume object workspace adoption",
        description:
          "The server recovered the exact interrupted adoption command. Review its read-only identity and resume it without creating a new command.",
        button: "Resume workspace adoption",
        busy: "Resuming adoption…",
      };
}

export function WorkspaceHeaderIdentity({
  status,
}: {
  status: ObjectWorkspaceStatus;
}) {
  if (status.workspace === null) {
    return (
      <div className="workspace-block" aria-label="Object workspace identity">
        <span>Startup-selected local workspace</span>
        <strong>Identity pending</strong>
        <small>No filesystem path is exposed</small>
      </div>
    );
  }
  return (
    <div className="workspace-block" aria-label="Object workspace identity">
      <span>Object workspace</span>
      <strong>{status.workspace.displayName}</strong>
      <small>Workspace ID {status.workspace.workspaceId}</small>
    </div>
  );
}

export function ObjectWorkspaceEvidence({
  workspace,
}: {
  workspace: ObjectWorkspaceRecord;
}) {
  return (
    <Card title="Object workspace identity" className="setup-focus-card">
      <p>
        This immutable descriptor identifies the startup-selected local
        workspace. It contains no server filesystem path.
      </p>
      <DefinitionGrid
        items={[
          {
            label: "Workspace display name",
            value: <Value>{workspace.displayName}</Value>,
          },
          {
            label: "Workspace ID",
            value: <Value>{workspace.workspaceId}</Value>,
          },
          {
            label: "Migration object name or scope",
            value: <Value>{workspace.migrationObjectNameOrScope}</Value>,
          },
          {
            label: "Creator / operator attribution",
            value: <Value>{workspace.creatorAttribution}</Value>,
          },
          {
            label: "Established by",
            value: <Value>{workspace.establishedBy}</Value>,
          },
          {
            label: "Descriptor generation",
            value: <Value>{workspace.generation}</Value>,
          },
          {
            label: "Created",
            value: <Value>{workspace.createdAt}</Value>,
          },
          {
            label: "Descriptor hash",
            value: <Hash>{workspace.descriptorHash}</Hash>,
          },
        ]}
      />
    </Card>
  );
}

function WorkspaceActionError({ error }: { error: ContractError | null }) {
  if (error === null) return null;
  return (
    <div className="notice notice--critical" role="alert">
      <strong>{error.code}</strong>
      <p>{error.message}</p>
      {error.code === "NETWORK_UNCERTAIN" ? (
        <p>
          Refresh the durable workspace state before retrying. This screen keeps
          the unchanged retry identity, and any durable pending command reopens
          as an exact read-only resume.
        </p>
      ) : null}
    </div>
  );
}

export function ObjectWorkspaceGate({
  status,
}: {
  status: ObjectWorkspaceStatus;
}) {
  const action = status.requiredAction;
  const pendingCommand = status.pendingCommand;
  const identity = useRef<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ContractError | null>(null);

  if (action === null) {
    return null;
  }
  const copy =
    pendingCommand === null ? actionCopy(action) : resumeActionCopy(action);

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      let command: ObjectWorkspaceCommand;
      if (pendingCommand !== null) {
        command = pendingCommand;
      } else {
        const form = new FormData(event.currentTarget);
        identity.current ??= idempotencyKey(
          action === "INITIALIZE"
            ? "initialize-object-workspace"
            : "adopt-object-workspace",
        );
        command = {
          schemaVersion:
            action === "INITIALIZE"
              ? OBJECT_WORKSPACE_COMMAND_SCHEMAS.initialize
              : OBJECT_WORKSPACE_COMMAND_SCHEMAS.adopt,
          expectedStateHash: status.stateHash,
          displayName: formText(form, "displayName"),
          migrationObjectNameOrScope: formText(
            form,
            "migrationObjectNameOrScope",
          ),
          creatorAttribution: formText(form, "creatorAttribution"),
          idempotencyKey: identity.current,
        };
      }
      await submitObjectWorkspaceCommand(command);
      identity.current = null;
    } catch (caught) {
      setError(safeError(caught));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title={copy.title} className="setup-focus-card">
      <p>{copy.description}</p>
      <p className="muted">{status.unavailableReason}</p>
      <form
        className="setup-form"
        onSubmit={(event) => void submit(event)}
        key={pendingCommand?.idempotencyKey ?? "new-workspace-command"}
      >
        <label>
          Workspace display name
          <input
            name="displayName"
            required
            maxLength={256}
            autoComplete="off"
            placeholder={
              pendingCommand === null
                ? "Customer readiness workspace"
                : undefined
            }
            readOnly={pendingCommand !== null}
            value={pendingCommand?.displayName}
          />
        </label>
        <label>
          Migration object name or scope
          <input
            name="migrationObjectNameOrScope"
            required
            maxLength={256}
            autoComplete="off"
            placeholder={
              pendingCommand === null ? "Customer master readiness" : undefined
            }
            readOnly={pendingCommand !== null}
            value={pendingCommand?.migrationObjectNameOrScope}
          />
        </label>
        <label>
          Creator / operator attribution
          <input
            name="creatorAttribution"
            required
            maxLength={256}
            autoComplete="off"
            placeholder={
              pendingCommand === null ? "Named local operator" : undefined
            }
            readOnly={pendingCommand !== null}
            value={pendingCommand?.creatorAttribution}
          />
        </label>
        <div className="authority-acknowledgment">
          <strong>
            {pendingCommand === null
              ? "Local workspace identity only"
              : "Exact pending command recovered"}
          </strong>
          {pendingCommand === null ? (
            <p>
              This action uses the server-selected folder. It accepts no browser
              path and grants no target, publication, or external-transmission
              authority.
            </p>
          ) : (
            <p>
              These identity fields are read-only. Resume reuses the original
              state and retry identity returned by the server; it grants no new
              authority.
            </p>
          )}
        </div>
        <WorkspaceActionError error={error} />
        <button type="submit" className="button" disabled={busy}>
          {busy ? copy.busy : copy.button}
        </button>
      </form>
    </Card>
  );
}

export function ActiveObjectWorkspace({
  status,
  children,
}: {
  status: ObjectWorkspaceStatus;
  children: ReactNode;
}) {
  if (status.workspace === null) return <ObjectWorkspaceGate status={status} />;
  return (
    <>
      <ObjectWorkspaceEvidence workspace={status.workspace} />
      {children}
    </>
  );
}
