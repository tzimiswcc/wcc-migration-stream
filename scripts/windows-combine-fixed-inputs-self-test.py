#!/usr/bin/env python3
"""Synthetic fixed-input admission tests; no process, HTTP or artifact operation."""
from pathlib import Path
import copy
import datetime as dt
import importlib.util
import re
import subprocess
import unittest
import urllib.request
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('fixed_pair',Path(__file__).with_name('windows-combine-fixed-inputs.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class AdmissionTests(unittest.TestCase):
 def setUp(self):
  self.process=patch.object(subprocess,'Popen',side_effect=AssertionError('process forbidden'));self.process.start()
  self.http=patch.object(urllib.request,'build_opener',side_effect=AssertionError('HTTP forbidden'));self.http.start()
  self.now=dt.datetime(2026,10,30,14,tzinfo=dt.timezone.utc)
  self.run={'id':m.RUN,'head_sha':m.BASE,'head_branch':'main','event':'workflow_dispatch','run_attempt':1,'status':'completed','conclusion':'failure','repository':{'full_name':m.REPO}}
  self.jobs={'total_count':3,'jobs':[{'id':pin['job'],'run_id':m.RUN,'name':'native-'+arch,'status':'completed','conclusion':'success'} for arch,pin in m.INPUTS.items()]+[{'id':124076541567,'run_id':m.RUN,'name':'combine','status':'completed','conclusion':'failure'}]}
  self.artifacts={arch:{'id':pin['id'],'name':'migration-stream-installer-native-'+arch+'-'+str(m.RUN),'expired':False,'size_in_bytes':pin['bytes'],'digest':'sha256:'+pin['digest'],'expires_at':pin['expires'],'workflow_run':{'id':m.RUN,'repository_id':1329975731,'head_repository_id':1329975731,'head_sha':m.BASE,'head_branch':'main'}} for arch,pin in m.INPUTS.items()}
  self.env={'GITHUB_EVENT_NAME':'workflow_dispatch','GITHUB_RUN_ATTEMPT':'1','GITHUB_REPOSITORY':m.REPO,'GITHUB_REF':'refs/heads/main','GITHUB_WORKFLOW_REF':m.REPO+'/.github/workflows/windows-combine-fixed-inputs.yml@refs/heads/main','REUSE_FIXED_NATIVE_PAIR':'true','GITHUB_SHA':'a'*40,'EXPECTED_SOURCE_COMMIT':'a'*40}
  self.baseline={name:'100644 blob '+'0'*40 for name in ('Cargo.toml','src/core.rs','scripts/package-windows-desktop.py','scripts/windows-desktop-transfer.py','native/windows/desktop-installer.nsi.in',*m.FIXED_DIAGNOSTICS)}
  self.current=dict(self.baseline);self.current.update(m.FIXED_DIAGNOSTICS);self.current['scripts/windows-combine-fixed-inputs.py']='100644 blob '+'1'*40
 def tearDown(self):self.http.stop();self.process.stop()
 def producer(self):return m.producer_admission(self.run,self.jobs,self.artifacts,self.now)
 def source(self):return m.source_admission(self.env,'a'*40,'b'*40,'a'*40,True,self.baseline,self.current)
 def test_exact_pair_returns_original_product_and_distinct_tooling(self):
  self.assertEqual(self.producer(),{'sourceCommit':m.BASE,'sourceTree':m.TREE,'sourceSnapshotSha256':m.SNAPSHOT,'nativeRunId':str(m.RUN),'runAttempt':1})
  self.assertEqual(self.source(),{'commit':'a'*40,'tree':'b'*40});self.assertNotEqual('a'*40,m.BASE)
 def test_wrong_source_repository_run_attempt_and_event_refuse(self):
  for key,value in [('head_sha','f'*40),('id',m.RUN+1),('head_branch','topic'),('run_attempt',2),('run_attempt',True),('event','push'),('status','in_progress'),('conclusion','success'),('repository',{'full_name':'other/repo'})]:
   original=copy.deepcopy(self.run);self.run[key]=value
   with self.assertRaises(m.Refused):self.producer()
   self.run=original
 def test_wrong_native_job_tuple_or_non_success_refuses(self):
  for key,value in [('id',123),('run_id',123),('name','native-arm64'),('conclusion','failure'),('status','queued')]:
   old=copy.deepcopy(self.jobs);self.jobs['jobs'][0][key]=value
   with self.assertRaises(m.Refused):self.producer()
   self.jobs=old
  self.jobs['jobs'].append(self.jobs['jobs'][0])
  with self.assertRaises(m.Refused):self.producer()
 def test_wrong_artifact_id_digest_bytes_expiry_or_owner_refuses(self):
  for arch in m.INPUTS:
   for key,value in [('id',123),('digest','sha256:'+'0'*64),('size_in_bytes',1),('expired',True),('name','foreign'),('expires_at','2027-01-01T00:00:00Z'),('workflow_run',{'id':m.RUN,'head_sha':'f'*40})]:
    old=copy.deepcopy(self.artifacts);self.artifacts[arch][key]=value
    with self.assertRaises(m.Refused):self.producer()
    self.artifacts=old
 def test_expiry_exact_margin_boundary_and_unknown_clock_refuse(self):
  earliest=dt.datetime.fromisoformat(m.INPUTS['x64']['expires'].replace('Z','+00:00'))
  self.now=earliest-dt.timedelta(minutes=20,seconds=1);self.producer()
  for now in (earliest-dt.timedelta(minutes=20),earliest,earliest+dt.timedelta(seconds=1),earliest.replace(tzinfo=None)):
   self.now=now
   with self.assertRaises(m.Refused):self.producer()
 def test_mixed_architecture_artifact_pair_refuses(self):
  self.artifacts['x64'],self.artifacts['arm64']=self.artifacts['arm64'],self.artifacts['x64']
  with self.assertRaises(m.Refused):self.producer()
 def test_tooling_main_expected_sha_workflow_and_opt_in_are_exact(self):
  for key,value in [('GITHUB_REF','refs/heads/topic'),('EXPECTED_SOURCE_COMMIT','f'*40),('GITHUB_SHA','f'*40),('GITHUB_WORKFLOW_REF','foreign'),('REUSE_FIXED_NATIVE_PAIR','false'),('GITHUB_RUN_ATTEMPT','2'),('GITHUB_EVENT_NAME','push')]:
   old=dict(self.env);self.env[key]=value
   with self.assertRaises(m.Refused):self.source()
   self.env=old
  for main,clean in [('f'*40,True),('a'*40,False)]:
   with self.assertRaises(m.Refused):m.source_admission(self.env,'a'*40,'b'*40,main,clean,self.baseline,self.current)
 def test_product_core_packager_template_unknown_additions_and_deletions_refuse(self):
  for name in ('src/core.rs','Cargo.toml','scripts/package-windows-desktop.py','scripts/windows-desktop-transfer.py','native/windows/desktop-installer.nsi.in','unknown.txt'):
   old=dict(self.current);self.current[name]='100644 blob '+'f'*40
   with self.assertRaises(m.Refused):self.source()
   self.current=old
  del self.current['src/core.rs']
  with self.assertRaises(m.Refused):self.source()
 def test_exact_unchanged_diagnostic_pins_cannot_drift(self):
  for name in m.FIXED_DIAGNOSTICS:
   old=dict(self.current);self.current[name]='100755 blob '+'f'*40
   with self.assertRaises(m.Refused):self.source()
   self.current=old
 def test_no_arbitrary_inputs_redirect_fallback_or_raw_error_output(self):
  source=Path(m.__file__).read_text()
  self.assertIn("require(sys.argv[1:]==['--admit'])",source)
  self.assertIn('urllib.request.ProxyHandler({})',source)
  with self.assertRaises(m.Refused):m.NoRedirect().redirect_request(None,None,None,None,None,None)
  catch=source.rsplit(' except Exception:',1)[1]
  self.assertEqual(catch.strip(),'''print('{"status":"FIXED_NATIVE_PAIR_REFUSED"}');raise SystemExit(1)''')
  self.assertNotIn('PRIVATE_SENTINEL',catch)
 def test_workflow_pins_closed_sources_downloads_and_no_output_publication(self):
  source=(ROOT/'.github/workflows/windows-combine-fixed-inputs.yml').read_text()
  self.assertIn('default: false',source);self.assertIn('timeout-minutes: 15',source)
  self.assertIn("github.ref == 'refs/heads/main'",source);self.assertIn('github.run_attempt == 1',source)
  self.assertEqual(source.count('uses:'),3);self.assertEqual(source.count('digest-mismatch: error'),2)
  for pin in m.INPUTS.values():self.assertIn('artifact-ids: '+str(pin['id']),source)
  self.assertLess(source.index('-Operation AdmitTransfers'),source.index('uses: actions/download-artifact@'))
  self.assertEqual(source.count('run-id: 41057681963'),2)
  for forbidden in ('upload-artifact','workflow_call:','schedule:','push:','build-assisted','Native -','setup-node','continue-on-error','actions: write'):
   self.assertNotIn(forbidden,source)
  self.assertEqual(source.count('run: |'),2)
  for block in source.split('run: |')[1:]:self.assertLess(len(block),21000)
 def test_real_qualifier_preserves_default_and_original_product_binding(self):
  s=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
  self.assertIn('$productCommit=$env:GITHUB_SHA',s);self.assertIn('$productTree=$desktopTree',s)
  self.assertIn("$desktopOperation -cin @('AdmitTransfers','Combine')",s)
  self.assertIn("'--source-commit',$productCommit,'--source-tree',$productTree",s)
  self.assertIn('$nativeReceipt.runId -is [string]',s);self.assertIn('$fixedPairAdmission.productBuild.nativeRunId',s)
  self.assertIn("$evidence['productBuild']=$fixedPairAdmission.productBuild",s)
  self.assertIn("$evidence['tooling']=$fixedPairAdmission.tooling",s)
  self.assertRegex(s,r'\$admissionText = @\(&[^\n]+\)\n    \$script:desktopExitCode=\$LASTEXITCODE\n    \$env:GH_TOKEN=\$null')
  self.assertIn('$drive.Free -ge 12GB',s);self.assertIn('$memory.TotalVisibleMemorySize * 1KB) -ge 8GB',s)
  self.assertIn("'--receipt-sha'",s);self.assertIn('installerExecuted=$false;setupUploaded=$false',s)

if __name__=='__main__':unittest.main()
