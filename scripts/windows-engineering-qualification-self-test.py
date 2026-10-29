#!/usr/bin/env python3
"""Narrow static/mutation guards; never execute PowerShell, tools or products.

These checks protect the reviewed selection, dispatch and evidence contracts.
They are not a general YAML/PowerShell parser or hosted artifact qualification.
"""
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BUILDER = ROOT / "scripts/build-assisted-runtime-bundle.ps1"
WORKFLOW = ROOT / ".github/workflows/windows-engineering-layout-qualification.yml"
CORPUS = ROOT / "config/release-corpus.json"
DEFAULT_BINARIES = [
    "migration-stream-local-runtime.exe", "migration-stream.exe",
    "migration-stream-workbench.exe", "migration-stream-bundle.exe",
]
ENGINEERING_BINARIES = [
    "migration-stream-document-open.exe", "migration-stream-windows-engineering-layout.exe",
]
EVIDENCE_FIELDS = {
    "schemaVersion", "status", "runId", "runAttempt", "stage", "commandExitCode", "sourceCommit", "sourceTree",
    "target", "runnerImageVersion", "nodeVersion", "npmVersion", "rustVersion",
    "pythonVersion", "toolSha256", "sourceSnapshotSha256", "frontendAssetsSha256",
    "documentLauncherSha256", "engineeringAssemblerSha256", "runtimeManifestSha256",
    "runtimeManifestFileSha256", "runtimeLauncherSha256", "runtimeControllerSha256", "integrityCliSha256",
    "workbenchSha256", "runtimeMemberCount", "runtimeMemberBytes",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_contract(builder, workflow, corpus):
    selector = re.search(
        r"(?ms)^function Get-PrivateReleaseBinaryNames\(\[switch\]\$IncludeEngineeringTools\) \{(.*?)^\}",
        builder,
    )
    require(selector is not None, "fixed selection helper missing")
    default, optional = selector[1].split("if ($IncludeEngineeringTools)", 1)
    require(re.findall(r"'([^']+\.exe)'", default) == DEFAULT_BINARIES, "default selection drift")
    require(re.findall(r"'([^']+\.exe)'", optional) == ENGINEERING_BINARIES, "opt-in selection drift")
    require("if ($RetainEngineeringTools -and -not $RetainedBuildRoot)" in builder,
            "retained-root admission missing")
    require(builder.index("if ($RetainEngineeringTools -and -not $RetainedBuildRoot)")
            < builder.index("Add-Type"), "admission must precede helper setup")
    require("Get-PrivateReleaseBinaryNames -IncludeEngineeringTools:$IncludeEngineeringTools" in builder,
            "bounded copier must use selection")
    require("-IncludeEngineeringTools:$RetainEngineeringTools" in builder,
            "builder must explicitly pass opt-in")
    require("Get-PrivateReleaseBinaryNames -IncludeEngineeringTools:$RetainEngineeringTools" in builder,
            "PE checks must use same selection")
    require("[long]$maximumMemberBytes = 128MB" in builder
            and "[long]$maximumTotalBytes = 512MB" in builder, "copy bounds changed")
    events = workflow.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
    require(re.findall(r"^  ([a-z_]+):", events, re.M) == ["workflow_dispatch"], "manual trigger required")
    require("      expected_source_commit:" in events and "        required: true" in events,
            "exact commit input required")
    require("permissions:\n  contents: read\n" in workflow, "read-only permission required")
    require("runs-on: windows-2025" in workflow and "timeout-minutes: 45" in workflow,
            "finite native runner lane required")
    require("cancel-in-progress: false" in workflow, "do not auto-cancel owned build")
    uses = re.findall(r"uses: ([^\s]+)", workflow)
    require(uses == [
        "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
        "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020",
    ], "unexpected action/pin or artifact publication")
    require("persist-credentials: false" in workflow and "ref: ${{ github.sha }}" in workflow,
            "exact checkout without persisted credentials required")
    require("$env:GITHUB_SHA -cne $env:EXPECTED_SOURCE_COMMIT" in workflow,
            "event/expected commit admission missing")
    require("github.sha == inputs.expected_source_commit && github.run_attempt == 1" in workflow,
            "pre-provisioning exact source/attempt guard required")
    require("$env:GITHUB_RUN_ATTEMPT -cne '1'" in workflow, "fresh manual attempt required")
    require("--profile','minimal'" in workflow and "'install','1.99.1'" in workflow,
            "Rust pin missing")
    require("'v24.22.1'" in workflow and "'npm@12.2.1'" in workflow,
            "Node/npm pins missing")
    require("$pythonNumber.Major -ne 3 -or $pythonNumber.Minor -lt 10" in workflow,
            "explicit existing Python prerequisite missing")
    require("$env:npm_config_fetch_retries = '0'" in workflow
            and "$env:CARGO_NET_RETRY = '0'" in workflow, "no command retry requested")
    cache = workflow.index("Invoke-QualificationTool 'cargo-cache'")
    offline = workflow.index("$env:CARGO_NET_OFFLINE = 'true'")
    build = workflow.index("$qualificationMetadata.stage = 'runtime-build'")
    require(cache < offline < build and "$env:npm_config_offline = 'true'" in workflow,
            "online cache then resolver-offline build required")
    require("-NoArchive `" in workflow and "-RetainEngineeringTools *>> $buildLog" in workflow,
            "exact opt-in no-archive driver call required")
    require("--expected-launcher-sha256" in workflow
            and "'--engineering-policy','local-unsigned-engineering'" in workflow,
            "fixed engineering policy/hash contract missing")
    for forbidden in (
        "upload-artifact", "Start-Process", "Invoke-Expression", "Start-Sleep",
        "-AllowUncommittedSnapshot", "Get-ChildItem Env:", "$env:GH_TOKEN",
        "$env:GITHUB_TOKEN", "Remove-Item", "workflow_run:", "schedule:",
    ):
        require(forbidden not in workflow, "unexpected execution/disclosure surface")
    block = workflow.split("$qualificationMetadata = [ordered]@{", 1)[1].split(
        "          }\n          function", 1)[0]
    fields = set(re.findall(r"^            ([A-Za-z][A-Za-z0-9]*) =", block, re.M))
    require(fields == EVIDENCE_FIELDS, "evidence fields must remain allowlisted")
    for output in re.findall(r"Write-Output ([^\n]+)", workflow):
        require(output.startswith("('QUALIFICATION stage=' + $Stage)")
                or output.startswith("'QUALIFICATION stage=runtime-build'")
                or output.startswith("'QUALIFICATION evidence refused'")
                or output == "$qualificationEvidence", "raw evidence output refused")
    require("$qualificationEvidence.Length -gt 8192" in workflow,
            "bounded final evidence required")
    require(workflow.count("[IO.FileMode]::CreateNew") >= 4,
            "private logs/config/evidence must reserve without overwrite")
    require("& $Program @Arguments *>> $privateLog" in workflow,
            "private command capture must not truncate prior content")
    for path in (
        ".github/workflows/windows-engineering-layout-qualification.yml",
        "scripts/windows-engineering-qualification-self-test.py",
    ):
        require(path in corpus["include"], "exact new source allowlist entry missing")


MAXIMUM_WORKFLOW_BYTES = 64 * 1024
RUN_DELIMITER = (
    "      - name: One private build and file-only engineering assembly\n"
    "        shell: pwsh\n"
    "        run: |\n"
)


def extract_workflow_powershell(workflow):
    require(len(workflow.encode("utf-8")) <= MAXIMUM_WORKFLOW_BYTES, "workflow size bound")
    workflow = workflow.replace("\r\n", "\n")
    require("\r" not in workflow and workflow.count(RUN_DELIMITER) == 1,
            "exact qualification run delimiter required")
    require(workflow.count("        run: |\n") == 1, "only one qualification body allowed")
    lines = workflow.split(RUN_DELIMITER, 1)[1].splitlines()
    require(lines and all(not line or line.startswith("          ") for line in lines),
            "qualification body indentation or trailing step refused")
    body = "\n".join(line[10:] if line else "" for line in lines) + "\n"
    require("$qualificationMetadata = [ordered]@{" in body, "qualification body marker required")
    return body


def write_workflow_powershell(workflow_path, destination):
    require(destination.is_absolute(), "static parse destination must be absolute")
    with workflow_path.open("rb") as source:
        raw = source.read(MAXIMUM_WORKFLOW_BYTES + 1)
    require(len(raw) <= MAXIMUM_WORKFLOW_BYTES, "workflow size bound")
    body = extract_workflow_powershell(raw.decode("utf-8", errors="strict"))
    # Exclusive creation refuses existing files and dangling links. Never execute.
    with destination.open("x", encoding="utf-8", newline="\n") as output:
        output.write(body)


class QualificationContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.builder = BUILDER.read_text(encoding="utf-8")
        cls.workflow = WORKFLOW.read_text(encoding="utf-8")
        cls.corpus = json.loads(CORPUS.read_text(encoding="utf-8"))

    def reject(self, *, builder=None, workflow=None, corpus=None):
        with self.assertRaises(ValueError):
            check_contract(builder or self.builder, workflow or self.workflow, corpus or self.corpus)

    def test_current_source_contract(self):
        check_contract(self.builder, self.workflow, self.corpus)

    def test_default_and_optional_names_are_distinct_closed_lists(self):
        self.reject(builder=self.builder.replace("'migration-stream-workbench.exe'", "'unexpected.exe'", 1))
        self.reject(builder=self.builder.replace("'migration-stream-document-open.exe'", "'unexpected.exe'", 1))

    def test_opt_in_cannot_lose_retained_root_or_copy_pe_pairing(self):
        self.reject(builder=self.builder.replace("-and -not $RetainedBuildRoot", "-and $false", 1))
        self.reject(builder=self.builder.replace("Get-PrivateReleaseBinaryNames -IncludeEngineeringTools:$IncludeEngineeringTools", "@()", 1))
        self.reject(builder=self.builder.replace("Get-PrivateReleaseBinaryNames -IncludeEngineeringTools:$RetainEngineeringTools", "@()", 1))

    def test_copy_bounds_cannot_expand(self):
        self.reject(builder=self.builder.replace("$maximumTotalBytes = 512MB", "$maximumTotalBytes = 1GB", 1))

    def test_no_automatic_event_or_write_permission(self):
        self.reject(workflow=self.workflow.replace("  workflow_dispatch:", "  push:\n  workflow_dispatch:", 1))
        self.reject(workflow=self.workflow.replace("contents: read", "contents: write", 1))

    def test_actions_remain_pinned_without_upload(self):
        self.reject(workflow=self.workflow.replace("actions/setup-node@820762786026740c76f36085b0efc47a31fe5020", "actions/setup-node@main", 1))
        self.reject(workflow=self.workflow + "\n      - uses: actions/upload-artifact@fixed\n")

    def test_commit_and_tool_prerequisites_are_not_silent_fallbacks(self):
        self.reject(workflow=self.workflow.replace("$env:GITHUB_SHA -cne $env:EXPECTED_SOURCE_COMMIT", "$false", 1))
        self.reject(workflow=self.workflow.replace("$env:GITHUB_RUN_ATTEMPT -cne '1'", "$false", 1))
        self.reject(workflow=self.workflow.replace("'v24.22.1'", "'unknown'", 1))
        self.reject(workflow=self.workflow.replace("$pythonNumber.Minor -lt 10", "$pythonNumber.Minor -lt 0", 1))

    def test_offline_phase_and_no_retry_are_explicit(self):
        self.reject(workflow=self.workflow.replace("$env:CARGO_NET_OFFLINE = 'true'", "$env:CARGO_NET_OFFLINE = 'false'", 1))
        self.reject(workflow=self.workflow.replace("$env:CARGO_NET_RETRY = '0'", "$env:CARGO_NET_RETRY = '3'", 1))

    def test_no_archive_or_verification_bypass(self):
        self.reject(workflow=self.workflow.replace("-NoArchive `", "`", 1))
        self.reject(workflow=self.workflow.replace("'local-unsigned-engineering'", "'release'", 1))

    def test_raw_output_and_extra_metadata_are_refused(self):
        self.reject(workflow=self.workflow + "\n          Write-Output $env:PATH\n")
        self.reject(workflow=self.workflow.replace("            stage = 'admission'", "            stage = 'admission'\n            rawPath = $env:RUNNER_TEMP", 1))

    def test_evidence_bound_and_first_use_reservation_remain(self):
        self.reject(workflow=self.workflow.replace("$qualificationEvidence.Length -gt 8192", "$false", 1))
        self.reject(workflow=self.workflow.replace("[IO.FileMode]::CreateNew", "[IO.FileMode]::Create"))

    def test_exact_new_paths_must_be_in_corpus(self):
        corpus = {**self.corpus, "include": [p for p in self.corpus["include"] if p != WORKFLOW.relative_to(ROOT).as_posix()]}
        self.reject(corpus=corpus)


    def test_exact_workflow_body_extraction_is_source_only(self):
        body = extract_workflow_powershell(self.workflow)
        self.assertTrue(body.startswith("$ErrorActionPreference"))
        self.assertIn("-RetainEngineeringTools", body)
        self.assertNotIn("runs-on:", body)
        self.assertEqual(extract_workflow_powershell(self.workflow.replace("\n", "\r\n")), body)

    def test_extraction_refuses_ambiguity_trailing_steps_indentation_and_size(self):
        for source in (
            self.workflow + RUN_DELIMITER,
            self.workflow + "      - name: unexpected next step\n",
            self.workflow.replace("          $ErrorActionPreference", "         $ErrorActionPreference", 1),
            self.workflow.replace("One private build and file-only engineering assembly", "other", 1),
            self.workflow + "x" * MAXIMUM_WORKFLOW_BYTES,
        ):
            with self.assertRaises(ValueError):
                extract_workflow_powershell(source)

    def test_extraction_writes_only_fresh_synthetic_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "synthetic.yml", root / "parse-only.ps1"
            source.write_text(self.workflow, encoding="utf-8")
            write_workflow_powershell(source, output)
            captured = output.read_bytes()
            self.assertEqual(captured.decode(), extract_workflow_powershell(self.workflow))
            with self.assertRaises(FileExistsError):
                write_workflow_powershell(source, output)
            self.assertEqual(output.read_bytes(), captured)
            with self.assertRaises(ValueError):
                write_workflow_powershell(source, Path("relative.ps1"))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--write-workflow-powershell":
        write_workflow_powershell(WORKFLOW, Path(sys.argv[2]))
    else:
        unittest.main()
