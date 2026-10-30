#!/usr/bin/env python3
"""Tiny synthetic ZIP/contract checks. No PowerShell, native or product process."""
from pathlib import Path
import hashlib
import importlib.util
import json
import re
import struct
import stat
import os
import sys
from types import SimpleNamespace
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
    def windows_interfaces(self,path_drift=None,fd_drift=None,identity_drift=None,archive_drift=False):
        # Real tiny files/bytes, independently modeled CPython3.12 named/fd views.
        original_lstat=Path.lstat;original_fstat=os.fstat
        identities={original_lstat(p).st_ino:p for p in (self.archive,self.receipt)}
        calls={self.archive:0,self.receipt:0};state={'extracted':False}
        def row(path,info,opened):
            values={name:getattr(info,name) for name in ('st_dev','st_ino','st_size','st_nlink','st_mtime_ns')}
            values.update(st_mode=stat.S_IFREG|0o666,st_ctime_ns=500 if opened else 300,
                          st_birthtime_ns=300,st_file_attributes=0x20,st_reparse_tag=0)
            if path==self.receipt and not archive_drift:
                if not opened and path_drift and calls[path]>=2:values.update(path_drift)
                if opened and fd_drift and calls[path]>=2:values.update(fd_drift)
                if opened and identity_drift:values.update(identity_drift)
            if path==self.archive and archive_drift and state['extracted']:
                if not opened and path_drift:values.update(path_drift)
                if opened and fd_drift:values.update(fd_drift)
            return SimpleNamespace(**values)
        def named(path):
            info=original_lstat(path)
            return row(path,info,False) if path in calls else info
        def opened(fd):
            info=original_fstat(fd);path=identities.get(info.st_ino)
            if path is None:return info
            calls[path]+=1;return row(path,info,True)
        original_bind=m._identity.bind_file_interfaces
        original_stage=m.tempfile.mkdtemp
        def stage(*args,**kwargs):
            value=original_stage(*args,**kwargs);state['extracted']=True;return value
        return (patch.object(Path,'lstat',named),patch.object(m.os,'fstat',opened),
                patch.object(m._identity,'bind_file_interfaces',side_effect=lambda n,o:original_bind(n,o,windows=True)),
                patch.object(m.tempfile,'mkdtemp',side_effect=stage))

    def test_closed_transfer_failure_fields_reach_qualifier_without_raw_output(self):
        with self.assertRaises(m.Refusal) as caught:m.json_value(b'PRIVATE_SENTINEL')
        self.assertEqual(caught.exception.evidence,{'status':'REFUSED','phase':'receipt-json','category':'format'})
        self.assertNotIn('PRIVATE',json.dumps(caught.exception.evidence))
        source=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
        start=source.index("$transferPhase='unknown';$transferCategory='unknown'")
        end=source.index(" 'G23'",start);block=source[start:end]
        self.assertIn('$transferFailure.phase -is [string]',block)
        self.assertIn('$transferFailure.category -is [string]',block)
        self.assertIn("$safeTransferFailure=[ordered]@{status='REFUSED';phase=$transferPhase;category=$transferCategory}",block)
        self.assertIn("Write-Output ('PASSIVE_TRANSFER_FAILURE '+($safeTransferFailure | ConvertTo-Json -Compress))",block)
        self.assertNotIn('Write-Output $result',block);self.assertNotIn('$_.',block)

    def test_windows312_receipt_ctime_difference_preserves_stable_bytes(self):
        raw=self.receipt.read_bytes()
        a,b,c,d=self.windows_interfaces()
        with a,b,c,d:self.assertEqual(m.read_small(self.receipt,m.MAX_RECEIPT),raw)
    def test_receipt_named_interface_ctime_drift_refuses(self):
        a,b,c,d=self.windows_interfaces(path_drift={'st_ctime_ns':301})
        with a,b,c,d,self.assertRaises(m.Refusal) as caught:m.read_small(self.receipt,m.MAX_RECEIPT)
        self.assertEqual(caught.exception.evidence,{'status':'REFUSED','phase':'receipt-read','category':'freshness'})
    def test_receipt_descriptor_interface_ctime_drift_refuses(self):
        a,b,c,d=self.windows_interfaces(fd_drift={'st_ctime_ns':501})
        with a,b,c,d,self.assertRaises(m.Refusal):m.read_small(self.receipt,m.MAX_RECEIPT)
    def test_receipt_cross_interface_identity_and_link_mutations_refuse(self):
        for change in ({'st_ino':1},{'st_dev':0},{'st_mtime_ns':1},{'st_nlink':2},{'st_file_attributes':0x400},{'st_birthtime_ns':301}):
            a,b,c,d=self.windows_interfaces(identity_drift=change)
            with a,b,c,d,self.assertRaises(m.Refusal):m.read_small(self.receipt,m.MAX_RECEIPT)
    def test_windows312_full_tiny_archive_replay_accepts_independent_interfaces(self):
        a,b,c,d=self.windows_interfaces()
        with a,b,c,d:self.assertEqual(self.run_unpack()['status'],'PASSIVE_TRANSFER_ONLY')
        self.assertEqual((self.dest/'engineering-layout/Runtime/tiny').read_bytes(),b'tiny')
    def test_archive_named_and_descriptor_freshness_drift_refuse_before_publication(self):
        for changed in ('path','fd'):
            a,b,c,d=self.windows_interfaces(path_drift={'st_ctime_ns':301} if changed=='path' else None,
                                             fd_drift={'st_ctime_ns':501} if changed=='fd' else None,archive_drift=True)
            with a,b,c,d,self.assertRaises(m.Refusal) as caught:self.run_unpack()
            self.assertEqual(caught.exception.evidence,{'status':'REFUSED','phase':'archive-read','category':'freshness'})
            self.assertFalse(self.dest.exists())

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

def native_cli_fixture():
    # Hosted Windows source proof only. Fixed Python helper, synthetic private
    # ZIP/JSON; never a Runtime/controller/compiler/app/installer executable.
    if os.name!='nt':raise SystemExit('native synthetic transfer CLI requires Windows')
    case=TransferTests('test_both_native_receipt_tuples_passively_extract_without_process')
    case.setUp()
    try:
        case.guard.stop()
        args=[sys.executable,'-I','-B',str(ROOT/'scripts/windows-desktop-transfer.py'),
              '--archive',str(case.archive),'--archive-sha',hashlib.sha256(case.archive.read_bytes()).hexdigest(),
              '--receipt',str(case.receipt),'--receipt-sha',hashlib.sha256(case.receipt.read_bytes()).hexdigest(),
              '--destination',str(case.dest),'--architecture',case.arch,'--source-commit',case.commit,'--source-tree',case.tree]
        result=subprocess.run(args,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                              timeout=20,check=False,env={'SystemRoot':os.environ['SystemRoot']})
        if result.returncode!=0 or len(result.stdout)>8192 or len(result.stderr)>8192:raise ValueError('synthetic CLI refused')
        value=json.loads(result.stdout)
        if value.get('status')!='PASSIVE_TRANSFER_ONLY' or (case.dest/'engineering-layout/Runtime/tiny').read_bytes()!=b'tiny':raise ValueError('synthetic CLI refused')
        print('PASS native Windows synthetic passive transfer CLI')
    finally:case.tearDown()

if __name__=='__main__':
    if sys.argv[1:]==['--native-cli-only']:native_cli_fixture()
    else:unittest.main()
