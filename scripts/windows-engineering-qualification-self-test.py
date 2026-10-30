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
DRIVER = ROOT / "scripts/windows-engineering-qualification.ps1"
HELPER = ROOT / "scripts/windows-engineering-preview.ps1"
SOURCE_CI = ROOT / ".github/workflows/public-source-ci.yml"
DEFAULT_BINARIES = [
    "migration-stream-local-runtime.exe", "migration-stream.exe",
    "migration-stream-workbench.exe", "migration-stream-bundle.exe",
]
ENGINEERING_BINARIES = [
    "migration-stream-document-open.exe", "migration-stream-windows-engineering-layout.exe",
]
EVIDENCE_FIELDS = {
    "schemaVersion", "status", "runId", "runAttempt", "stage", "commandExitCode", "sourceCommit", "sourceTree",
    "architecture", "target", "runnerImageVersion", "nodeVersion", "npmVersion", "rustVersion",
    "pythonVersion", "toolSha256", "sourceSnapshotSha256", "frontendAssetsSha256",
    "documentLauncherSha256", "engineeringAssemblerSha256", "runtimeManifestSha256",
    "runtimeManifestFileSha256", "runtimeLauncherSha256", "runtimeControllerSha256", "integrityCliSha256",
    "workbenchSha256", "runtimeMemberCount", "runtimeMemberBytes",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_architecture_contract(workflow, driver, helper, source_ci):
    choice = """      architecture:
        description: Native engineering validation lane (not an end-user delivery choice)
        required: true
        default: x64
        type: choice
        options:
          - x64
          - arm64
"""
    require(choice in workflow, "closed native architecture choice required")
    require("(inputs.architecture == 'x64' || inputs.architecture == 'arm64')" in workflow,
            "unknown architecture must skip before provisioning")
    require("runs-on: ${{ inputs.architecture == 'arm64' && 'windows-11-arm' || 'windows-2025' }}" in workflow,
            "closed native runner map required")
    require("architecture: ${{ inputs.architecture }}" in workflow
            and "QUALIFICATION_ARCHITECTURE: ${{ inputs.architecture }}" in workflow,
            "native Node and operator architecture selection required")
    require("'1.99.1-aarch64-pc-windows-msvc' || '1.99.1-x86_64-pc-windows-msvc'" in workflow,
            "explicit Rust host toolchain map required")
    for row in (
        "architecture='x64'; target='x86_64-pc-windows-msvc'; processor='AMD64'; runtime='X64'; node='x64'; runner='windows-2025'; artifact='migration-stream-local-windows-x64'; toolchain='1.99.1-x86_64-pc-windows-msvc'; peMachine=0x8664",
        "architecture='arm64'; target='aarch64-pc-windows-msvc'; processor='ARM64'; runtime='Arm64'; node='arm64'; runner='windows-11-arm'; artifact='migration-stream-local-windows-arm64'; toolchain='1.99.1-aarch64-pc-windows-msvc'; peMachine=0xAA64",
    ):
        require(row in helper, "mixed native target tuple refused")
    require("Assert-PreviewNativeHost $targetTuple $env:PROCESSOR_ARCHITECTURE" in driver
            and "::OSArchitecture.ToString()" in driver and "::ProcessArchitecture.ToString()" in driver,
            "actual native host/process admission required")
    require("$env:RUSTUP_TOOLCHAIN -cne $targetTuple.toolchain" in driver
            and "Assert-PreviewRustHost $targetTuple $rustVersion" in driver
            and "'host: '+$Tuple.target" in helper,
            "actual pinned Rust host admission required")
    require("process.versions.node+' '+process.arch" in driver
            and "'v24.22.1 '+$targetTuple.node" in driver,
            "actual native Node architecture required")
    for required in ("--target',$targetTuple.target", "-Target $targetTuple.target", "$targetTuple.artifact",
                     "$verified.target.architecture -cne $targetTuple.architecture", "$verified.target.targetTriple -cne $targetTuple.target",
                     "preview_target=", "preview_architecture="):
        require(required in driver, "build/manifest/export target binding required")
    require("Join-Path $qualificationRepository ('target/'+$targetTuple.target+'/release')" in driver,
            "release binary lookup must follow selected native target")
    require(not re.search(r"target/(?:x86_64|aarch64)-pc-windows-msvc/release", driver),
            "hardcoded native release path refused")
    require("migration-stream-engineering-candidate-${{ inputs.architecture }}-${{ github.sha }}" in workflow
            and "'migration-stream-engineering-candidate-'+$targetTuple.architecture+'-'" in driver
            and "'migration-stream-engineering-candidate-'+$targetTuple.architecture+'-'" in workflow,
            "archive name and selected architecture must agree")
    require("$env:PREVIEW_EXPECTED_TARGET -cne $targetTuple.target" in workflow
            and "target=$result.target" in workflow and "architecture=$result.architecture" in workflow,
            "explicit target handoff evidence required")
    require("$buildTuple=Get-PreviewBuildTarget $receipt.build" in helper
            and "$buildTuple.architecture -ceq $tuple.architecture -and $buildTuple.target -ceq $tuple.target" in helper
            and "Assert-PreviewVerificationTarget $receipt.runtime.target $receipt.build" in helper
            and "Assert-PreviewVerificationTarget $v.target $Build" in helper,
            "receipt/runtime target must match explicit expected lane")
    require("-Architecture $targetTuple.architecture" in driver
            and "-Architecture $targetTuple.architecture" in workflow,
            "no receipt-selected target authority")
    require(helper.count("$buildTuple.architecture -ceq $tuple.architecture -and $buildTuple.target -ceq $tuple.target") == 2,
            "both Pack and Extract must bind scalar build tuple to expected lane")
    for required in ("$Build.architecture -is [string] -and $Build.target -is [string]",
                     "$Target.os -is [string] -and $Target.architecture -is [string] -and $Target.targetTriple -is [string]",
                     "$tuple=Get-PreviewBuildTarget $Value", "$buildTuple=Get-PreviewBuildTarget $Build"):
        require(required in helper, "primitive scalar target admission required")
    production = helper.split("function Invoke-PreviewRuntimeVerification", 1)[1].split("function Read-PreviewBlock", 1)[0]
    host_call = "Assert-PreviewNativeHost $tuple $env:PROCESSOR_ARCHITECTURE ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString())"
    require(host_call in production and production.index(host_call) < production.index("& $controller verify"),
            "production verifier must refuse nonnative host before execution")
    entry = helper.split("if ($MyInvocation.InvocationName -ne '.')", 1)[1]
    entry_call = host_call.replace("$tuple", "$entryTuple")
    require(entry_call in entry and entry.index(entry_call) < entry.index("$archivePath = Get-PreviewFullPath"),
            "public helper must refuse nonnative host before path operations")
    for required in ("architecture: x64", "runner: windows-2025", "toolchain: 1.99.1-x86_64-pc-windows-msvc",
                     "architecture: arm64", "runner: windows-11-arm", "toolchain: 1.99.1-aarch64-pc-windows-msvc",
                     "runs-on: ${{ matrix.runner }}", "Assert-PreviewNativeHost $tuple $env:PROCESSOR_ARCHITECTURE",
                     "::OSArchitecture.ToString()", "::ProcessArchitecture.ToString()",
                     "rustc +${{ matrix.toolchain }} -vV", "Assert-PreviewRustHost $tuple $versionLines",
                     "cargo +${{ matrix.toolchain }} test", "cargo +${{ matrix.toolchain }} clippy"):
        require(required in source_ci, "paired native source validation required")


def check_contract(builder, workflow, corpus, driver, helper, source_ci):
    check_architecture_contract(workflow, driver, helper, source_ci)
    # Validate the actual server-evaluated bodies before reconstructing the
    # external driver at its original indentation for unchanged semantic guards.
    bodies = workflow_powershell_bodies(workflow)
    require(bodies[0] == QUALIFICATION_WRAPPER, "fixed external driver wrapper required")
    require(len(driver.encode("utf-8")) <= 64 * 1024, "bounded external driver source")
    require(driver.startswith("$ErrorActionPreference"), "driver admission marker required")
    wrapper_indented = "".join("          " + line for line in QUALIFICATION_WRAPPER.splitlines(keepends=True))
    driver_indented = "".join("          " + line for line in driver.splitlines(keepends=True))
    require(workflow.count(wrapper_indented) == 1, "unambiguous driver invocation required")
    workflow = workflow.replace(wrapper_indented, driver_indented, 1)
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
    require("timeout-minutes: 45" in workflow,
            "finite native runner lane required")
    require("cancel-in-progress: false" in workflow, "do not auto-cancel owned build")
    uses = re.findall(r"uses: ([^\s]+)", workflow)
    require(uses == [
        "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
        "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020",
        "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9",
        "actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333",
    ], "unexpected action/pin or artifact publication")
    require("persist-credentials: false" in workflow and "ref: ${{ github.sha }}" in workflow,
            "exact checkout without persisted credentials required")
    require(workflow.count("$env:GITHUB_SHA -cne $env:EXPECTED_SOURCE_COMMIT") == 2,
            "event/expected commit admission missing")
    require("github.sha == inputs.expected_source_commit && github.run_attempt == 1" in workflow,
            "pre-provisioning exact source/attempt guard required")
    require(workflow.count("$env:GITHUB_RUN_ATTEMPT -cne '1'") == 2, "fresh manual attempt required")
    require("--profile','minimal'" in workflow and "'install',$targetTuple.toolchain" in workflow,
            "Rust pin missing")
    require("'v24.22.1 '" in workflow and "'npm@12.2.1'" in workflow,
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
        "Start-Process", "Invoke-Expression", "Start-Sleep",
        "-AllowUncommittedSnapshot", "Get-ChildItem Env:", "$env:GH_TOKEN",
        "$env:GITHUB_TOKEN", "Remove-Item", "workflow_run:", "schedule:",
    ):
        require(forbidden not in workflow, "unexpected execution/disclosure surface")
    require("      publish_engineering_preview:" in events and "        default: false" in events,
            "preview publication must default false")
    require("switch -CaseSensitive ($env:PUBLISH_ENGINEERING_PREVIEW)" in workflow
            and "'false' { $false }" in workflow and "'true' { $true }" in workflow
            and "[bool]$env:PUBLISH_ENGINEERING_PREVIEW" not in workflow,
            "exact preview bool parsing required")
    require("if ($previewEnabled -eq $true)" in workflow, "helper/export must be guarded")
    guard = "if: ${{ success() && inputs.publish_engineering_preview == true && github.run_attempt == 1 && github.sha == inputs.expected_source_commit && steps.qualify.outputs.preview_candidate_ready == 'true' }}"
    require(workflow.count(guard) == 3, "upload/download/handoff guards required")
    for required in ("archive: false", "overwrite: false", "if-no-files-found: error", "retention-days: 7",
                     "artifact-ids: ${{ steps.candidate-upload.outputs.artifact-id }}", "skip-decompress: true", "digest-mismatch: error"):
        require(required in workflow, "opaque exact-ID candidate transport required")
    require("name: migration-stream" not in workflow, "single-file artifact name comes from candidate filename")
    require(workflow.index("$result = ($resultText") < workflow.index("status='QUALIFIED_ENGINEERING_PREVIEW'"),
            "qualified evidence must follow download replay")
    block = workflow.split("$qualificationMetadata = [ordered]@{", 1)[1].split(
        "          }\n          function", 1)[0]
    fields = set(re.findall(r"^            ([A-Za-z][A-Za-z0-9]*) =", block, re.M))
    require(fields == EVIDENCE_FIELDS, "evidence fields must remain allowlisted")
    for output in re.findall(r"Write-Output ([^\n]+)", workflow):
        require(output.startswith("('QUALIFICATION stage=' + $Stage)")
                or output.startswith("'QUALIFICATION stage=runtime-build'")
                or output.startswith("'QUALIFICATION evidence refused'")
                or output.startswith("'Engineering candidate ")
                or output.startswith("($previewEvidence | ConvertTo-Json -Compress)")
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
        "scripts/windows-engineering-qualification.ps1",
        "scripts/windows-engineering-preview.ps1",
        "scripts/windows-engineering-preview.Tests.ps1",
    ):
        require(path in corpus["include"], "exact new source allowlist entry missing")


MAXIMUM_WORKFLOW_BYTES = 64 * 1024
MAXIMUM_RUN_CHARACTERS = 21000
QUALIFICATION_WRAPPER = (
    "$ErrorActionPreference = 'Stop'\n"
    "& ./scripts/windows-engineering-qualification.ps1\n"
    "if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n"
)


def validate_run_body(body):
    # Conservatively count astral Unicode as two units; reviewed blocks are ASCII.
    require(len(body.encode("utf-16-le")) // 2 <= MAXIMUM_RUN_CHARACTERS,
            "GitHub run block exceeds 21000 characters")


RUN_DELIMITER = (
    "      - name: One private build and file-only engineering assembly\n"
    "        id: qualify\n"
    "        shell: pwsh\n"
    "        run: |\n"
)


def workflow_powershell_bodies(workflow):
    require(len(workflow.encode("utf-8")) <= MAXIMUM_WORKFLOW_BYTES, "workflow size bound")
    workflow = workflow.replace("\r\n", "\n")
    require("\r" not in workflow and workflow.count(RUN_DELIMITER) == 1,
            "exact qualification run delimiter required")
    # Closed workflow subset: also reject unnamed, inline, folded, quoted or
    # flow-style run keys, rather than scanning only our literal delimiters.
    # Exactly the six reviewed steps also refuse escaped/alternate key spellings
    # in an extra unnamed entry, without trying to implement general YAML.
    require(len(re.findall(r"^      -[ \t]", workflow, re.M)) == 6,
            "only six reviewed workflow steps allowed")
    run_keys = re.findall(r"(?<![A-Za-z0-9_-])(?:run|'run'|\"run\")[ \t]*:", workflow)
    require(run_keys == ["run:", "run:"], "only two exact run keys allowed")
    require(workflow.count("        run: |\n") == 2, "only two reviewed source bodies allowed")
    names = re.findall(r"^      - name: (.+)$", workflow, re.M)
    require(names == ["One private build and file-only engineering assembly",
                      "Upload one explicitly unqualified engineering candidate",
                      "Download only this engineering candidate without decompression",
                      "Verify downloaded engineering candidate before preview handoff"], "unknown step refused")
    chunks = workflow.split("        run: |\n")[1:]
    bodies = []
    for chunk in chunks:
        lines = []
        for line in chunk.splitlines():
            if line and not line.startswith("          "):
                break
            lines.append(line[10:] if line else "")
        require(lines, "empty reviewed source body")
        body = "\n".join(lines) + "\n"
        validate_run_body(body)
        bodies.append(body)
    require(bodies[0] == QUALIFICATION_WRAPPER and bodies[1].startswith("# PREVIEW_DOWNLOAD_REPLAY"),
            "exact reviewed run block order required")
    return bodies


def extract_workflow_powershell(workflow):
    bodies = workflow_powershell_bodies(workflow)
    # The source contract checker separately validates exact actions and guards.
    body = bodies[0] + "\n# Separate download replay source body; parse only.\n" + bodies[1]
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
        cls.driver = DRIVER.read_text(encoding="utf-8")
        cls.helper = HELPER.read_text(encoding="utf-8")
        cls.source_ci = SOURCE_CI.read_text(encoding="utf-8")

    def reject(self, *, builder=None, workflow=None, corpus=None, driver=None, helper=None, source_ci=None):
        with self.assertRaises(ValueError):
            check_contract(builder or self.builder, workflow or self.workflow, corpus or self.corpus, driver or self.driver, helper or self.helper, source_ci or self.source_ci)

    def test_current_source_contract(self):
        check_contract(self.builder, self.workflow, self.corpus, self.driver, self.helper, self.source_ci)

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
        self.reject(driver=self.driver.replace("$env:GITHUB_SHA -cne $env:EXPECTED_SOURCE_COMMIT", "$false", 1))
        self.reject(driver=self.driver.replace("$env:GITHUB_RUN_ATTEMPT -cne '1'", "$false", 1))
        self.reject(driver=self.driver.replace("'v24.22.1 '", "'unknown'", 1))
        self.reject(driver=self.driver.replace("$pythonNumber.Minor -lt 10", "$pythonNumber.Minor -lt 0", 1))

    def test_offline_phase_and_no_retry_are_explicit(self):
        self.reject(driver=self.driver.replace("$env:CARGO_NET_OFFLINE = 'true'", "$env:CARGO_NET_OFFLINE = 'false'", 1))
        self.reject(driver=self.driver.replace("$env:CARGO_NET_RETRY = '0'", "$env:CARGO_NET_RETRY = '3'", 1))

    def test_no_archive_or_verification_bypass(self):
        self.reject(driver=self.driver.replace("-NoArchive `", "`", 1))
        self.reject(driver=self.driver.replace("'local-unsigned-engineering'", "'release'", 1))

    def test_raw_output_and_extra_metadata_are_refused(self):
        self.reject(workflow=self.workflow + "\n          Write-Output $env:PATH\n")
        self.reject(driver=self.driver.replace("  stage = 'admission'", "  stage = 'admission'\n  rawPath = $env:RUNNER_TEMP", 1))

    def test_evidence_bound_and_first_use_reservation_remain(self):
        self.reject(driver=self.driver.replace("$qualificationEvidence.Length -gt 8192", "$false", 1))
        self.reject(driver=self.driver.replace("[IO.FileMode]::CreateNew", "[IO.FileMode]::Create"))

    def test_exact_new_paths_must_be_in_corpus(self):
        corpus = {**self.corpus, "include": [p for p in self.corpus["include"] if p != WORKFLOW.relative_to(ROOT).as_posix()]}
        self.reject(corpus=corpus)


    def test_exact_workflow_body_extraction_is_source_only(self):
        body = extract_workflow_powershell(self.workflow)
        self.assertTrue(body.startswith("$ErrorActionPreference"))
        self.assertIn("& ./scripts/windows-engineering-qualification.ps1", body)
        self.assertNotIn("-RetainEngineeringTools", body)
        self.assertIn("-RetainEngineeringTools", self.driver)
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


    def test_server_run_character_limit_boundary_and_oversize(self):
        validate_run_body("x" * 21000)
        with self.assertRaises(ValueError):
            validate_run_body("x" * 21001)
        validate_run_body("😀" * 10500)
        with self.assertRaises(ValueError):
            validate_run_body("😀" * 10500 + "x")
        replay = workflow_powershell_bodies(self.workflow)[1]
        padded = replay + "#" * (21000 - len(replay) - 1) + "\n"
        self.assertEqual(len(padded), 21000)
        old = "".join("          " + line for line in replay.splitlines(keepends=True))
        replacement = "".join("          " + line for line in padded.splitlines(keepends=True))
        self.assertEqual(len(workflow_powershell_bodies(self.workflow.replace(old, replacement))[1]), 21000)
        with self.assertRaises(ValueError):
            workflow_powershell_bodies(self.workflow.replace(old, replacement + "          #\n"))

    def test_closed_native_architecture_mapping_cannot_drift(self):
        self.reject(workflow=self.workflow.replace("        default: x64", "        default: arm64", 1))
        self.reject(workflow=self.workflow.replace("'windows-11-arm'", "'windows-2025'", 1))
        self.reject(helper=self.helper.replace("target='aarch64-pc-windows-msvc'", "target='x86_64-pc-windows-msvc'", 1))
        self.reject(driver=self.driver.replace("Assert-PreviewNativeHost $targetTuple", "Write-Output $targetTuple", 1))
        self.reject(driver=self.driver.replace("Assert-PreviewRustHost $targetTuple", "Write-Output $targetTuple", 1))
        self.reject(source_ci=self.source_ci.replace("rustc +${{ matrix.toolchain }} -vV", "rustc --version", 1))
        self.reject(driver=self.driver.replace("('target/'+$targetTuple.target+'/release')", "'target/x86_64-pc-windows-msvc/release'", 1))

    def test_expected_preview_target_and_export_cannot_be_lost(self):
        self.reject(helper=self.helper.replace("$buildTuple.target -ceq $tuple.target", "$true", 1))
        self.reject(workflow=self.workflow.replace("$env:PREVIEW_EXPECTED_TARGET -cne $targetTuple.target", "$false", 1))
        self.reject(driver=self.driver.replace("preview_target=", "other_target=", 1))
        self.reject(helper=self.helper.replace("$Build.architecture -is [string] -and $Build.target -is [string]", "$true", 1))
        host_call = "    Assert-PreviewNativeHost $tuple $env:PROCESSOR_ARCHITECTURE ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) ([Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString())\n"
        self.reject(helper=self.helper.replace(host_call, "", 1))
        late = self.helper.replace(host_call, "", 1).replace("    $output = @(& $controller verify 2>&1)\n", "    $output = @(& $controller verify 2>&1)\n" + host_call, 1)
        self.reject(helper=late)
        self.reject(helper=self.helper.replace("Assert-PreviewNativeHost $entryTuple", "Write-Output $entryTuple", 1))
        self.reject(workflow=self.workflow.replace("migration-stream-engineering-candidate-${{ inputs.architecture }}", "migration-stream-engineering-candidate-x64", 1))

    def test_unadmitted_run_keys_cannot_bypass_server_limit(self):
        for step in (
            "      - run: " + "x" * 21001 + "\n",
            "      - run: echo synthetic\n",
            "      - run: >\n          echo synthetic\n",
            "      - 'run': echo synthetic\n",
            '      - {run: "echo synthetic"}\n',
            '      - {"r\\u0075n": "echo synthetic"}\n',
        ):
            source = self.workflow + step
            self.assertLess(len(source.encode("utf-8")), MAXIMUM_WORKFLOW_BYTES)
            with self.assertRaises(ValueError):
                extract_workflow_powershell(source)
            self.reject(workflow=source)

    def test_external_driver_invocation_is_fixed(self):
        self.reject(workflow=self.workflow.replace("& ./scripts/windows-engineering-qualification.ps1", "& ./scripts/other.ps1", 1))
        self.reject(driver=self.driver.replace("$ErrorActionPreference", "$other", 1))

    def test_preview_cannot_lose_default_false_or_transport_guards(self):
        self.reject(workflow=self.workflow.replace("        default: false", "        default: true", 1))
        self.reject(driver=self.driver.replace("if ($previewEnabled -eq $true)", "if ($true)", 1))
        self.reject(workflow=self.workflow.replace("inputs.publish_engineering_preview == true", "true", 1))
        self.reject(workflow=self.workflow.replace("skip-decompress: true", "skip-decompress: false", 1))
        self.reject(workflow=self.workflow.replace("digest-mismatch: error", "digest-mismatch: warn", 1))
        self.reject(workflow=self.workflow.replace("retention-days: 7", "retention-days: 90", 1))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--write-workflow-powershell":
        write_workflow_powershell(WORKFLOW, Path(sys.argv[2]))
    else:
        unittest.main()
