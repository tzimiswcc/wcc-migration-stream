#!/usr/bin/env python3
"""Offline tiny candidate/receipt fixtures; forbid process and HTTP operations."""
from pathlib import Path
import contextlib
import copy
import datetime as dt
import hashlib
import importlib.util
import io
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import urllib.request
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('candidate_under_test',ROOT/'scripts/windows-engineering-candidate.py')
m=importlib.util.module_from_spec(spec);sys.modules[spec.name]=m;spec.loader.exec_module(m)

class CandidateTests(unittest.TestCase):
 def setUp(self):
  self.process=patch.object(subprocess,'Popen',side_effect=AssertionError('process forbidden'));self.process.start()
  self.http=patch.object(urllib.request,'build_opener',side_effect=AssertionError('HTTP forbidden'));self.http.start()
  self.temp=tempfile.TemporaryDirectory(prefix='candidate-synthetic-');self.parent=Path(self.temp.name).resolve()
  self.compiled=self.parent/'installer';self.compiled.mkdir();self.output=self.parent/'engineering-candidate'
  self.compile_path=self.parent/'qualification-compile.json'
  self.ctx={'run':'12345','tooling':{'commit':'a'*40,'tree':'b'*40},'parent':self.parent}
  self.now=dt.datetime(2026,10,30,18,tzinfo=dt.timezone.utc)
  self.setup=bytearray(256);self.setup[:2]=b'MZ';struct.pack_into('<I',self.setup,60,64);self.setup[64:68]=b'PE\0\0';struct.pack_into('<H',self.setup,68,0x14c);struct.pack_into('<H',self.setup,88,0x10b)
  self.setup=bytes(self.setup);self.sha=hashlib.sha256(self.setup).hexdigest()
  self.product={'sourceCommit':m.f.BASE,'sourceTree':m.f.TREE,'sourceSnapshotSha256':m.f.SNAPSHOT,'nativeRunId':str(m.f.RUN),'runAttempt':1}
  version=__import__('re').findall(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"$',(ROOT/'Cargo.toml').read_text(),__import__('re').M)[0]
  self.package={'schemaVersion':'migration-stream/windows-desktop-package/v1',**{k:self.product[k] for k in ('sourceCommit','sourceTree','sourceSnapshotSha256')},'productVersion':version,'qualification':'PASSIVE_RECEIPT_BYTE_BINDING_ONLY','installer':{'bytes':256,'sha256':self.sha,'signingPerformed':False},'compilerSha256':m.n.MEMBER_PINS[m.n.COMPILER],'templateSha256':hashlib.sha256((ROOT/'native/windows/desktop-installer.nsi.in').read_bytes()).hexdigest(),'renderedScriptSha256':'c'*64,'nativeReceiptSha256':{a:p['receipt'] for a,p in m.f.INPUTS.items()},'runtimeManifestSha256':{'x64':'d'*64,'arm64':'e'*64},'installerExecutionQualified':False}
  self.proof={'schemaVersion':'migration-stream/windows-desktop-installer-qualification/v1','status':'COMPILED_PASSIVE_BYTES_ONLY',**{k:self.product[k] for k in ('sourceCommit','sourceTree','sourceSnapshotSha256')},'runId':'12345','runAttempt':1,'pythonVersion':'3.12.10','pythonSha256':'f'*64,'nsisArchiveSha256':m.n.ARCHIVE_SHA256,'nsisPreparationReceiptSha256':'1'*64,'nsisExeSha256':m.n.MEMBER_PINS[m.n.COMPILER],'nsisVersion':m.n.VERSION,'nsisVersionEvidence':'PINNED_ARCHIVE_RECEIPT','nsisFileVersion':None,'nsisResourceSha256':m.n.MEMBER_PINS,'installerSha256':self.sha,'installerBytes':256,'installerExecuted':False,'setupUploaded':False,'transferRetentionDays':1,'reusedInputs':True,'productBuild':self.product,'tooling':self.ctx['tooling']}
  for a,p in m.f.INPUTS.items():self.proof[a+'ArtifactId']=str(p['id']);self.proof[a+'ArtifactDigest']=p['digest']
  self.write_fixture()
  artifact={'id':42,'name':'migration-stream-engineering-candidate-12345','size_in_bytes':2048,'digest':'sha256:'+'2'*64,'expired':False,'created_at':'2026-10-30T17:59:00Z','expires_at':'2026-11-06T17:59:00Z','workflow_run':{'id':12345,'head_sha':'a'*40,'head_branch':'main','repository_id':1329975731,'head_repository_id':1329975731}}
  self.inventory={'total_count':1,'artifacts':[artifact]}
 def tearDown(self):self.http.stop();self.process.stop();self.temp.cleanup()
 def write_fixture(self):
  (self.compiled/m.FILES[0]).write_bytes(self.setup)
  (self.compiled/m.FILES[1]).write_bytes(m.canonical(self.package))
  self.compile_path.write_bytes(m.canonical(self.proof))
 def stage(self,**kw):
  return m.stage_candidate(self.compiled,self.compile_path,self.output,self.ctx,compile_sha=hashlib.sha256(self.compile_path.read_bytes()).hexdigest(),publish=lambda s,t:os.rename(s,t),**kw)
 def metadata(self):return m.artifact_admission(self.inventory,self.ctx,'42','2'*64,self.now)
 def copied(self):
  authority=self.stage();download=self.parent/'candidate-download';shutil.copytree(self.output,download)
  return authority,download
 def refusal(self,fn):
  with self.assertRaises((m.Refused,m.p.PackagingError,OSError,ValueError,TypeError,KeyError)):fn()
 def test_three_file_staging_and_passive_replay_preserve_prepublication_proof(self):
  old=self.compile_path.read_bytes();authority,download=self.copied()
  self.assertEqual(set(x.name for x in self.output.iterdir()),set(m.FILES))
  result=m.replay_candidate(download,authority,self.metadata(),self.ctx)
  self.assertEqual(result['status'],'ENGINEERING_CANDIDATE_PUBLISHED_PASSIVE_REPLAY')
  self.assertIs(result['setupUploaded'],True);self.assertIs(result['installerExecuted'],False)
  self.assertEqual(result['files'][m.FILES[0]],{'bytes':256,'sha256':self.sha})
  self.assertEqual(self.compile_path.read_bytes(),old);self.assertIs(json.loads(old)['setupUploaded'],False)
  self.assertNotIn('sourceRecords',result);self.assertNotIn(str(self.parent),json.dumps(result))
 def test_compiler_output_hash_is_external_authority(self):
  self.refusal(lambda:m.stage_candidate(self.compiled,self.compile_path,self.output,self.ctx,compile_sha='0'*64,publish=lambda s,t:os.rename(s,t)))
  self.assertFalse(self.output.exists())
 def test_refuses_missing_or_extra_receipt_fields_without_publication(self):
  for document in (self.package,self.proof):
   old=dict(document);document['privateExtra']='PRIVATE_SENTINEL';self.write_fixture();self.refusal(self.stage);self.assertFalse(self.output.exists());document.clear();document.update(old)
  self.write_fixture();self.compile_path.unlink();self.refusal(self.stage);self.assertFalse(self.output.exists())
 def test_product_tooling_run_tuple_and_phase_tamper_refuse(self):
  for key,value in [('sourceCommit','0'*40),('sourceTree','0'*40),('sourceSnapshotSha256','0'*64),('tooling',{'commit':'0'*40,'tree':'b'*40}),('runId','54321'),('runAttempt',2),('runAttempt',True),('status','FAILED'),('installerExecuted',True),('setupUploaded',True),('reusedInputs',False),('x64ArtifactId','12'),('arm64ArtifactDigest','0'*64),('productBuild',dict(self.product,runAttempt=True))]:
   with self.subTest(key=key,value=value):
    old=copy.deepcopy(self.proof);self.proof[key]=value;self.write_fixture();self.refusal(self.stage);self.assertFalse(self.output.exists());self.proof=old
 def test_compiler_template_licensing_tuple_and_installer_binding_refuse(self):
  for key,value in [('compilerSha256','0'*64),('templateSha256','0'*64),('nativeReceiptSha256',{'x64':'0'*64,'arm64':'0'*64}),('installerExecutionQualified',True),('qualification','EXECUTED'),('productVersion','99.0.0'),('installer',{'bytes':256,'sha256':'0'*64,'signingPerformed':False})]:
   old=copy.deepcopy(self.package);self.package[key]=value;self.write_fixture();self.refusal(self.stage);self.package=old
 def test_nsis_and_python_proof_are_closed_and_bounded(self):
  for key,value in [('nsisArchiveSha256','0'*64),('nsisExeSha256','0'*64),('nsisResourceSha256',{}),('nsisVersion','unknown'),('nsisVersionEvidence','UNVERIFIED'),('pythonVersion','PRIVATE_SENTINEL'),('pythonSha256','unknown'),('nsisFileVersion','PRIVATE_SENTINEL'),('transferRetentionDays',True)]:
   old=copy.deepcopy(self.proof);self.proof[key]=value;self.write_fixture();self.refusal(self.stage);self.proof=old
 def test_pe_and_setup_size_and_hash_binding_refuse(self):
  for data in (b'not a PE',self.setup[:68]+b'\x64\x86'+self.setup[70:],self.setup+b'extra'):
   (self.compiled/m.FILES[0]).write_bytes(data);self.refusal(self.stage);self.assertFalse(self.output.exists())
 def test_receipts_are_bounded_and_duplicate_json_keys_refuse(self):
  for raw in (b'{}'+b' '*16384,b'{"status":"x","Status":"PRIVATE_SENTINEL"}',b'[]',b'{'):
   self.compile_path.write_bytes(raw);self.refusal(self.stage);self.assertFalse(self.output.exists())
 def test_exact_file_set_refuses_unknown_case_alias_and_directories(self):
  for name in ('unknown','ENGINEERING-INSTALLER.JSON','nested'):
   item=self.compiled/name
   if name=='nested':item.mkdir()
   else:item.write_bytes(b'PRIVATE_SENTINEL')
   self.refusal(self.stage);self.assertFalse(self.output.exists())
   if item.is_dir():item.rmdir()
   else:item.unlink()
 def test_create_only_destination_and_overlap_preserve_sentinel(self):
  self.output.mkdir();sentinel=self.output/'sentinel';sentinel.write_bytes(b'kept')
  self.refusal(self.stage);self.assertEqual(sentinel.read_bytes(),b'kept')
  self.refusal(lambda:m.stage_candidate(self.compiled,self.compile_path,self.compiled,self.ctx,compile_sha=hashlib.sha256(self.compile_path.read_bytes()).hexdigest()))
 def test_hardlink_and_symlink_refuse_if_supported(self):
  setup=self.compiled/m.FILES[0];other=self.parent/'other';other.write_bytes(self.setup);setup.unlink()
  try:os.link(other,setup)
  except OSError:self.skipTest('hardlinks unavailable')
  self.refusal(self.stage);setup.unlink()
  try:setup.symlink_to(other)
  except OSError:self.skipTest('symlinks unavailable')
  self.refusal(self.stage)
 def test_marked_file_and_directory_stream_checks_are_required(self):
  for target in (self.compiled,self.compiled/m.FILES[0]):
   original=m.p.no_streams
   def check(path):
    if path==target:raise m.p.PackagingError('PRIVATE_SENTINEL')
    return original(path)
   with patch.object(m.p,'no_streams',side_effect=check):self.refusal(self.stage)
   self.assertFalse(self.output.exists())
 def test_change_during_staging_refuses_before_publish(self):
  original=m.read;changed=False
  def read(path,maximum):
   nonlocal changed
   result=original(path,maximum)
   if path.name==m.FILES[0] and path.parent!=self.compiled and not changed:
    changed=True;(self.compiled/m.FILES[0]).write_bytes(self.setup+b'changed')
   return result
  with patch.object(m,'read',side_effect=read):self.refusal(self.stage)
  self.assertFalse(self.output.exists())
 def test_fresh_source_and_stage_replacement_refuse_after_upload(self):
  authority,download=self.copied()
  for path in (self.compiled/m.FILES[0],self.output/m.FILES[1],self.compile_path):
   old=path.read_bytes();path.write_bytes(old+b' ');self.refusal(lambda:m.replay_candidate(download,authority,self.metadata(),self.ctx));path.write_bytes(old)
  # Restoration is still a changed file generation and is not accepted.
  self.refusal(lambda:m.replay_candidate(download,authority,self.metadata(),self.ctx))
 def test_download_tamper_extra_file_and_receipt_binding_refuse(self):
  authority,download=self.copied();target=download/m.FILES[0];old=target.read_bytes();target.write_bytes(old+b'changed')
  self.refusal(lambda:m.replay_candidate(download,authority,self.metadata(),self.ctx));target.write_bytes(old)
  extra=download/'unexpected';extra.write_bytes(b'private');self.refusal(lambda:m.replay_candidate(download,authority,self.metadata(),self.ctx));extra.unlink()
  proof=download/m.FILES[2];proof.write_bytes(b'{}');self.refusal(lambda:m.replay_candidate(download,authority,self.metadata(),self.ctx))
 def test_only_one_exact_current_run_artifact_is_admitted(self):
  value=self.metadata();self.assertEqual(value['artifactId'],'42');self.assertEqual(value['artifactDigest'],'2'*64)
  for inventory in ({'total_count':0,'artifacts':[]},{'total_count':2,'artifacts':self.inventory['artifacts']*2},{'total_count':True,'artifacts':self.inventory['artifacts']}):
   self.refusal(lambda:m.artifact_admission(inventory,self.ctx,'42','2'*64,self.now))
 def test_artifact_id_digest_name_size_expiry_and_owner_refuse(self):
  for key,value in [('id',43),('digest','sha256:'+'0'*64),('name','foreign'),('size_in_bytes',0),('size_in_bytes',True),('expired',True),('created_at','2026-10-31T18:00:00Z'),('expires_at','2026-11-07T17:59:00Z'),('workflow_run',{'id':12345,'head_sha':'0'*40})]:
   old=copy.deepcopy(self.inventory);self.inventory['artifacts'][0][key]=value;self.refusal(self.metadata);self.inventory=old
  self.refusal(lambda:m.artifact_admission(self.inventory,self.ctx,'42','2'*64,self.now.replace(tzinfo=None)))
 def test_fake_metadata_transport_is_current_run_bounded_and_no_redirect(self):
  seen=[]
  class Response:
   status=200
   def __enter__(self):return self
   def __exit__(self,*_):pass
   def read(self,limit):seen.append(limit);return b'{"total_count":0,"artifacts":[]}'
  class Opener:
   def open(self,request,timeout):seen.extend([request.full_url,timeout]);return Response()
  self.assertEqual(m.metadata_get(Opener(),'PRIVATE_TOKEN','12345',lambda:0),{'total_count':0,'artifacts':[]})
  self.assertEqual(seen,['https://api.github.com/repos/tzimiswcc/wcc-migration-stream/actions/runs/12345/artifacts?per_page=2',10,1048577])
  self.refusal(lambda:m.f.NoRedirect().redirect_request(None,None,None,None,None,None))
 def test_false_or_unknown_publication_flag_refuses_before_git_and_paths(self):
  for value in ('false','False','1','',None):
   with patch.object(m.os,'name','nt'),patch.object(m.f,'git',side_effect=AssertionError('git must not run')):
    self.refusal(lambda:m.context({'PUBLISH_ENGINEERING_CANDIDATE':value}))
 @unittest.skipUnless(os.name=='nt','native NTFS streams require Windows')
 def test_actual_ntfs_file_and_directory_streams_refuse_without_setup_execution(self):
  for target in (self.compiled,self.compiled/m.FILES[0]):
   stream=Path(str(target)+':candidate-synthetic')
   stream.write_bytes(b'fake alternate stream')
   try:self.refusal(self.stage);self.assertFalse(self.output.exists())
   finally:stream.unlink()
 def test_fake_metadata_status_size_json_and_deadline_refuse(self):
  class Response:
   status=200;data=b'{}'
   def __enter__(self):return self
   def __exit__(self,*_):pass
   def read(self,limit):return self.data
  class Opener:
   def open(self,*_,**__):return response
  response=Response()
  for status,data in ((403,b'PRIVATE_SENTINEL'),(200,b'x'*1048577),(200,b'{"a":1,"A":2}'),(200,b'PRIVATE_SENTINEL')):
   response.status=status;response.data=data
   self.refusal(lambda:m.metadata_get(Opener(),'PRIVATE_TOKEN','12345',lambda:0))
  response.status=200;response.data=b'{}';clock=iter((0,16))
  self.refusal(lambda:m.metadata_get(Opener(),'PRIVATE_TOKEN','12345',lambda:next(clock)))
 def test_cli_has_closed_errors_and_no_raw_exception_serialization(self):
  for error in (m.Refused('replay'),ValueError('PRIVATE_SENTINEL')):
   out=io.StringIO()
   with patch.object(m.sys,'argv',['subject','--replay']),patch.object(m,'context',side_effect=error),contextlib.redirect_stdout(out):self.assertEqual(m.main(),1)
   value=json.loads(out.getvalue());self.assertEqual(value['status'],'ENGINEERING_CANDIDATE_REFUSED');self.assertNotIn('PRIVATE',out.getvalue())
 def test_workflow_publication_is_default_off_exact_three_files_one_artifact(self):
  source=(ROOT/'.github/workflows/windows-combine-fixed-inputs.yml').read_text()
  self.assertEqual(source.count('default: false'),2);self.assertIn('timeout-minutes: 15',source);self.assertIn('github.run_attempt == 1',source)
  self.assertEqual(source.count('upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9'),1)
  self.assertEqual(source.count('download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333'),3)
  self.assertIn('retention-days: 7',source);self.assertIn('overwrite: false',source);self.assertIn('if-no-files-found: error',source)
  self.assertIn('artifact-ids: ${{ steps.upload_candidate.outputs.artifact-id }}',source);self.assertIn('run-id: ${{ github.run_id }}',source)
  self.assertEqual(source.count('/engineering-candidate/'),3)
  self.assertEqual(source.count("inputs.publish_engineering_candidate == true && steps.compile.outputs.compile_passed == 'true'"),5)
  for forbidden in ('workflow_call:','schedule:','push:','continue-on-error','actions: write','unblock','Start-Process'):
   self.assertNotIn(forbidden,source)
  for block in source.split('run: |')[1:]:self.assertLess(len(block.encode()),21000)
 def test_compile_proof_is_create_only_prepublication_and_source_ci_uses_fake_tests(self):
  driver=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
  self.assertIn('[IO.FileMode]::CreateNew',driver);self.assertIn("Output-Desktop 'compile_passed' 'true'",driver)
  self.assertLess(driver.index("$script:desktopStage='source-admission'"),driver.index("Output-Desktop 'compile_passed' 'true'"))
  self.assertIn('installerExecuted=$false;setupUploaded=$false',driver)
  self.assertIn("$receiptBytes=[Text.UTF8Encoding]::new($false).GetBytes($json+\"`n\")",driver)
  ci=(ROOT/'.github/workflows/public-source-ci.yml').read_text();self.assertEqual(ci.count('scripts/windows-engineering-candidate-self-test.py'),2)
  self.assertNotIn('publish_engineering_candidate:',ci)

if __name__=='__main__':unittest.main()
