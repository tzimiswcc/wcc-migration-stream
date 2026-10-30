#!/usr/bin/env python3
"""Tiny synthetic ZIP/contract checks. No PowerShell, native or product process."""
from pathlib import Path
import hashlib
import importlib.util
import json
import re
import struct
import subprocess
import tempfile
import unittest
import zipfile
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
SPEC=importlib.util.spec_from_file_location('desktop_transfer',Path(__file__).with_name('windows-desktop-transfer.py'))
m=importlib.util.module_from_spec(SPEC);SPEC.loader.exec_module(m)

def check_run_blocks(workflow):
    # Closed workflow, not general YAML: admit only the four reviewed literal
    # blocks. Any extra unnamed/inline/folded/flow run key refuses conservatively.
    keys=list(re.finditer(r"(?<![\w-])(?:run|['\"]run['\"])\s*:",workflow))
    lines=workflow.splitlines(keepends=True)
    admitted=[i for i,line in enumerate(lines) if line.rstrip('\n')=='        run: |']
    if len(keys)!=4 or len(admitted)!=4:raise ValueError('run shape refused')
    for index in admitted:
        body=[]
        for line in lines[index+1:]:
            if line.strip() and not line.startswith('          '):break
            body.append(line[10:] if line.startswith('          ') else line)
        if len(''.join(body))>21000:raise ValueError('run size refused')


def check_diagnostics(driver):
    # Source-only closed contract, not a PowerShell evaluator/parser or native proof.
    failure = "$failure=[ordered]@{status='REFUSED';stage=$script:desktopStage;guard=$script:desktopGuard;category=$script:desktopFailureCategory;exitCode=$script:desktopExitCode}"
    if failure not in driver:raise ValueError('failure fields refused')
    catch=driver.rsplit('} catch {',1)[1]
    if catch.strip() != (failure+"\n  Write-Output ('DESKTOP_QUALIFICATION_FAILURE '+($failure | ConvertTo-Json -Compress))\n  Write-Output 'Desktop installer qualification refused; partials and explicitly unqualified transfer artifacts preserved'\n  exit 1\n}").strip():raise ValueError('failure output refused')
    lines=driver.splitlines()
    commands=[]
    for index,line in enumerate(lines):
        if '=@(& ' in line:
            if lines[index+1].strip()!='$script:desktopExitCode=$LASTEXITCODE':raise ValueError('exit capture refused')
            commands.append(line.split('=@(& ',1)[1][:-1])
    if commands != ['git rev-parse HEAD',"git rev-parse 'HEAD^{tree}'",'git status --porcelain=v1 --untracked-files=all',
                     '$python -c \'import sys; assert sys.version_info[:2]>=(3,11); print("%d.%d.%d"%sys.version_info[:3])\'',
                     '$python @toolArguments','$python -B scripts/prepare-nsis-tool.py --destination $preparedDirectory',
                     '$python @toolArguments','$python -B scripts/prepare-nsis-tool.py --validate-prepared $preparedDirectory',
                     'git status --porcelain=v1 --untracked-files=all']:raise ValueError('commands changed')
    pairs=[("Set-DesktopStage 'disk-probe'",'$drive=Get-PSDrive'),
           ("Set-DesktopStage 'memory-probe'",'$memory=Get-CimInstance'),
           ("Set-DesktopStage 'disk-admission'",'Require-Desktop ($drive.Free -ge 12GB)'),
           ("Set-DesktopStage 'memory-admission'",'Require-Desktop (([long]$memory.TotalVisibleMemorySize * 1KB) -ge 8GB)'),
           ("Set-DesktopStage 'private-parent'",'$parent=Join-Path'),
           ("Set-DesktopStage 'python-selection'",'$python=(Get-Command'),
           ("Set-DesktopStage 'python-admission'",'$pythonVersion=@(&'),
           ("Set-DesktopStage ($arch+'-file-admission')",'$downloadInput=Join-Path'),
           ("Set-DesktopStage ($arch+'-passive-replay')",'      $result=@(&'),
           ("Set-DesktopStage 'nsis-preparation'",'$preparedText=@(&'),
           ("Set-DesktopStage 'installer-compile'",'    $result=@(&'),
           ("Set-DesktopStage 'postcompile-integrity'",'$replayedTool=@(&')]
    for stage,operation in pairs:
        if not re.search(re.escape(stage)+r'\n\s*'+re.escape(operation.lstrip()),driver):raise ValueError('stage ordering refused')
    admission=driver.index("if ($desktopOperation -ceq 'AdmitTransfers')")
    if not admission < driver.index('      return',admission) < driver.index("Set-DesktopStage 'resource-path'"):raise ValueError('admission return changed')


class TransferTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='desktop-transfer-synthetic-');self.root=Path(self.temp.name).resolve()
        self.archive=self.root/'native-layout.zip';self.receipt=self.root/'qualification.json';self.dest=self.root/'fresh';self.commit='a'*40;self.tree='b'*40
        self.guard=patch.object(subprocess,'Popen',side_effect=AssertionError('no process in synthetic source tests'));self.guard.start()
        self.make()
    def tearDown(self):self.guard.stop();self.temp.cleanup()
    def make(self,arch='x64',mutate=None,extra=None):
        build={'schemaVersion':'migration-stream/windows-engineering-qualification/v1','status':'PASSED','stage':'complete','commandExitCode':0,'runAttempt':1,'sourceCommit':self.commit,'sourceTree':self.tree,'architecture':arch,'target':m.TARGETS[arch],'runtimeMemberCount':1,'runtimeMemberBytes':4}
        for key in ('sourceSnapshotSha256','frontendAssetsSha256','documentLauncherSha256','engineeringAssemblerSha256','runtimeManifestSha256','runtimeManifestFileSha256','runtimeLauncherSha256','runtimeControllerSha256','integrityCliSha256','workbenchSha256'):build[key]='c'*64
        self.receipt.write_bytes(json.dumps(build).encode())
        selected={key:value for key,value in build.items() if key not in ('stage','commandExitCode','runAttempt')}
        rows=[{'path':'','type':'directory','bytes':0,'sha256':None},{'path':'Runtime','type':'directory','bytes':0,'sha256':None},{'path':'Runtime/tiny','type':'file','bytes':4,'sha256':hashlib.sha256(b'tiny').hexdigest()},{'path':'migration-stream-document-open.exe','type':'file','bytes':4,'sha256':hashlib.sha256(b'fake').hexdigest()}]
        value={'schemaVersion':'migration-stream/windows-engineering-preview/v1','build':selected,'runtime':{'target':{'os':'windows','architecture':arch,'targetTriple':m.TARGETS[arch]}},'entries':rows}
        if mutate:mutate(value)
        with zipfile.ZipFile(self.archive,'w',compression=zipfile.ZIP_STORED) as z:
            for name,data in [('engineering-layout/',b''),('engineering-layout/Runtime/',b''),('engineering-layout/Runtime/tiny',b'tiny'),('engineering-layout/migration-stream-document-open.exe',b'fake'),('preview-receipt.json',json.dumps(value).encode())]:z.writestr(name,data)
            if extra:z.writestr(*extra)
        self.arch=arch
    def run_unpack(self,**change):
        args=dict(archive=self.archive,archive_sha=hashlib.sha256(self.archive.read_bytes()).hexdigest(),receipt=self.receipt,receipt_sha=hashlib.sha256(self.receipt.read_bytes()).hexdigest(),destination=self.dest,architecture=self.arch,source_commit=self.commit,source_tree=self.tree);args.update(change);return m.unpack(**args)
    def test_both_native_receipt_tuples_passively_extract_without_process(self):
        for arch in ('x64','arm64'):
            self.make(arch);self.dest=self.root/arch
            self.assertEqual(self.run_unpack()['status'],'PASSIVE_TRANSFER_ONLY')
            self.assertEqual((self.dest/'engineering-layout/Runtime/tiny').read_bytes(),b'tiny')
    def test_external_hashes_refuse_before_destination(self):
        for key in ('archive_sha','receipt_sha'):
            with self.assertRaises(m.Refusal):self.run_unpack(**{key:'0'*64})
            self.assertFalse(self.dest.exists())
    def test_original_receipt_source_target_and_snapshot_binding(self):
        for field,value in [('source_commit','f'*40),('source_tree','f'*40),('architecture','arm64')]:
            with self.assertRaises(m.Refusal):self.run_unpack(**{field:value})
        self.make(mutate=lambda v:v['build'].update(sourceSnapshotSha256='f'*64))
        with self.assertRaises(m.Refusal):self.run_unpack()
    def test_no_overwrite_or_symlink_destination(self):
        self.dest.mkdir();(self.dest/'sentinel').write_bytes(b'kept')
        with self.assertRaises(m.Refusal):self.run_unpack()
        self.assertEqual((self.dest/'sentinel').read_bytes(),b'kept')
    def test_traversal_case_collision_ads_device_and_undeclared_members(self):
        for name in ['engineering-layout/../escape','engineering-layout/Runtime/TINY','engineering-layout/a:stream','engineering-layout/CON.txt','engineering-layout/undeclared']:
            with self.subTest(name=name):
                self.make(extra=(name,b'fake'))
                with self.assertRaises(m.Refusal):self.run_unpack()
                self.assertFalse(self.dest.exists())
    def test_receipt_member_hash_and_size_refusal(self):
        for change in [lambda v:v['entries'][2].update(sha256='0'*64),lambda v:v['entries'][2].update(bytes=1),lambda v:v['entries'][0].update(type='file',sha256='0'*64)]:
            self.make(mutate=change)
            with self.assertRaises((m.Refusal,OSError)):self.run_unpack()
            self.assertFalse(self.dest.exists())
    def test_central_local_signature_method_and_offset_refusal(self):
        raw=self.archive.read_bytes()
        for offset,payload in [(0,b'XX'),(8,struct.pack('<H',8)),(len(raw)-6,struct.pack('<I',0))]:
            data=bytearray(raw);data[offset:offset+len(payload)]=payload;self.archive.write_bytes(data)
            with self.assertRaises(m.Refusal):self.run_unpack()
            self.assertFalse(self.dest.exists())
    def test_comments_compression_and_link_attributes_refuse(self):
        with zipfile.ZipFile(self.archive,'a') as z:z.comment=b'not-admitted'
        with self.assertRaises(m.Refusal):self.run_unpack()
        self.make();data=bytearray(self.archive.read_bytes());offset=data.index(b'PK\x01\x02');struct.pack_into('<I',data,offset+38,0xA000<<16);self.archive.write_bytes(data)
        with self.assertRaises(m.Refusal):self.run_unpack()
    def test_count_size_depth_duplicate_json_caps(self):
        with self.assertRaises(m.Refusal):m.json_value(b'{"a":1,"A":2}')
        with self.assertRaises(m.Refusal):m.json_value(b'{}'+b' '*(m.MAX_RECEIPT+1))
        for name in ['engineering-layout/'+('/'.join(['x']*67)), 'engineering-layout/a\\b','engineering-layout/a.']:
            with self.assertRaises(m.Refusal):m.name_ok(name)
        with patch.object(m,'MAX_TOTAL',3),self.assertRaises(m.Refusal):self.run_unpack()
        with patch.object(m,'MAX_ENTRIES',4),self.assertRaises(m.Refusal):self.run_unpack()
    def test_fixed_manual_workflow_and_driver_contracts(self):
        workflow=(ROOT/'.github/workflows/windows-desktop-installer-qualification.yml').read_text();driver=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
        for name in ('native-x64:','native-arm64:','needs: [native-x64, native-arm64]','runs-on: windows-11-arm','runs-on: windows-2025','default: false','retention-days: 1','overwrite: false','digest-mismatch: error','persist-credentials: false','archive: true'):
            self.assertIn(name,workflow)
        self.assertEqual(workflow.count('uses: actions/upload-artifact@'),2)
        self.assertEqual(workflow.count('uses: actions/download-artifact@'),2)
        self.assertEqual(workflow.count('artifact-ids:'),2)
        self.assertLess(workflow.index('-Operation AdmitTransfers'),workflow.index('uses: actions/download-artifact@'))
        self.assertIn("$desktopOperation -ceq 'AdmitTransfers'",driver)
        self.assertNotIn('workflow_call:',workflow);self.assertNotIn('push:',workflow)
        self.assertNotIn('NSIS/makensis.exe',driver)
        self.assertIn("'Bin/makensis.exe'",driver)
        check_run_blocks(workflow)
        for name in ('. ./scripts/windows-engineering-qualification.ps1','PASSIVE_TRANSFER_ONLY','COMPILED_PASSIVE_BYTES_ONLY','scripts/prepare-nsis-tool.py','GetVersionInfo','setupUploaded=$false','installerExecuted=$false','--receipt-sha'):
            self.assertIn(name,driver)
        for forbidden in ('choco ','winget ','Invoke-WebRequest','ExecWait','Start-Process','Remove-Item','--skip-verify'):
            self.assertNotIn(forbidden,driver)
        self.assertIn('if ($desktopOperation -ceq',driver)
        self.assertIn("$env:PUBLISH_NATIVE_TRANSFERS -ceq 'true'",driver)

    def test_helper_parameter_cannot_replace_outer_operation(self):
        driver=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
        helper=(ROOT/'scripts/windows-engineering-preview.ps1').read_text()
        parameters=helper.split('function Assert-Preview',1)[0]
        self.assertRegex(parameters,r'\[string\]\$Operation\b')
        self.assertNotRegex(parameters,r'\$desktopOperation\b')
        capture=driver.index('$desktopOperation = $Operation')
        for source in ('. ./scripts/windows-engineering-qualification.ps1',
                       '. ./scripts/windows-engineering-preview.ps1'):
            self.assertLess(capture,driver.index(source))
        after_helper=driver[driver.index('. ./scripts/windows-engineering-preview.ps1'):]
        self.assertNotRegex(after_helper,r'if\s*\(\$Operation\b')
        self.assertIn("if ($desktopOperation -ceq 'Native')",driver)
        self.assertIn("if ($desktopOperation -ceq 'AdmitTransfers')",after_helper)

    def test_fixed_diagnostic_source_order_and_private_error_mutations(self):
        driver=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
        check_diagnostics(driver)
        mutations=[driver.replace('$script:desktopExitCode=$LASTEXITCODE','Write-Output "PRIVATE_SENTINEL"',1),
                   driver.replace("status='REFUSED';stage=", "message=$_.Exception.Message;status='REFUSED';stage=",1),
                   driver.replace("  exit 1\n}","  Write-Output $_\n  exit 1\n}",1),
                   driver.replace("Set-DesktopStage 'disk-probe'",'Write-Output "PRIVATE_SENTINEL"',1),
                   driver.replace('$drive.Free -ge 12GB','$drive.Free -ge 1GB',1),
                   driver.replace('      return','      Write-Output "PRIVATE_SENTINEL"',1),
                   driver.replace('$python -B scripts/prepare-nsis-tool.py --destination','$python -B scripts/other.py --destination',1)]
        for mutation in mutations:
            with self.assertRaises((ValueError,IndexError)):check_diagnostics(mutation)
        # No exception object or uncontrolled value participates in the catch output.
        catch=driver.rsplit('} catch {',1)[1]
        for forbidden in ('$_.', '$_ ', '$env:', '$python', '$parent', '$result', 'PRIVATE_SENTINEL'):
            self.assertNotIn(forbidden,catch)
        self.assertIn("[ValidatePattern('^G[0-9]{2}$')]",driver)
        self.assertIn("[ValidateSet('source-admission'",driver)

    def test_run_block_guard_rejects_unnamed_folded_flow_and_oversize_mutations(self):
        workflow=(ROOT/'.github/workflows/windows-desktop-installer-qualification.yml').read_text()
        for mutation in (workflow+'\n      - run: '+('x'*21001)+'\n',workflow+'\n      - run: echo short\n',workflow+'\n      - run: >\n          echo folded\n',workflow+'\n      - {run: echo flow}\n',workflow.replace('        run: |','        run: >',1),workflow.replace('        run: |','        run: |\n          '+('x'*21001),1)):
            with self.assertRaises(ValueError):check_run_blocks(mutation)

if __name__=='__main__':unittest.main()
