#!/usr/bin/env python3
"""Fixed three-file engineering candidate staging/replay. Never execute a file."""
from __future__ import annotations
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import time
import urllib.request

ROOT=Path(__file__).resolve().parents[1]
def load(name,file):
 spec=importlib.util.spec_from_file_location(name,ROOT/'scripts'/file);m=importlib.util.module_from_spec(spec);sys.modules[name]=m;spec.loader.exec_module(m);return m
p=load('candidate_packager','package-windows-desktop.py')
f=load('candidate_fixed_inputs','windows-combine-fixed-inputs.py')
n=load('candidate_nsis','prepare-nsis-tool.py')
FILES=('Migration Stream Setup.exe','engineering-installer.json','qualification-compile.json')
PHASES=frozenset(('context','receipts','stage','artifact-metadata','replay','output','unknown'))
class Refused(ValueError):
 def __init__(self,phase='unknown'):
  assert phase in PHASES
  self.evidence={'status':'ENGINEERING_CANDIDATE_REFUSED','phase':phase}
  super().__init__('engineering candidate refused')
def require(value,phase):
 if not value:raise Refused(phase)
def digest(value):return type(value) is str and re.fullmatch('[0-9a-f]{64}',value) is not None

def context(env):
 require(os.name=='nt' and sys.version_info>=(3,11),'context')
 require(env.get('PUBLISH_ENGINEERING_CANDIDATE')=='true','context')
 tooling=f.source_admission(env,f.git('rev-parse','HEAD'),f.git('rev-parse','HEAD^{tree}'),f.git('rev-parse','refs/remotes/origin/main'),not f.git('status','--porcelain=v1','--untracked-files=all'),f.entries(f.BASE),f.entries('HEAD'))
 run=env.get('GITHUB_RUN_ID','');require(re.fullmatch('[0-9]{1,20}',run) is not None,'context')
 parent=Path(env['RUNNER_TEMP'])/('migration-stream-desktop-combine-'+run)
 p.real_path(parent);require(parent.is_dir(),'context');p.no_streams(parent)
 return {'run':run,'tooling':tooling,'parent':parent}

def read(path,maximum):return p.file_record(path,maximum)
def canonical(value):return p.canonical(value)+b'\n'
def json_file(path):
 record,data=read(path,16384);return record,p.parse_json(data)
def stable(record):return p.parse_json(p.canonical(record))
def sources(ctx):
 return {name:ctx['parent']/('qualification-compile.json' if name==FILES[2] else 'installer/'+name) for name in FILES}
def file_set(directory,names,phase):
 p.real_path(directory);p.no_streams(directory);require(directory.is_dir() and {x.name for x in directory.iterdir()}==set(names),phase)
def fresh_sources(authority,ctx):
 file_set(ctx['parent']/'installer',FILES[:2],'replay')
 file_set(ctx['parent']/'engineering-candidate',FILES,'replay')
 require(set(authority.get('sourceRecords',{}))==set(FILES),'replay')
 for name,source in sources(ctx).items():
  maximum=p.MAX_SETUP if name==FILES[0] else 16384
  record,_=read(source,maximum);require(stable(record)==authority['sourceRecords'][name],'replay')
  staged,_=read(ctx['parent']/'engineering-candidate'/name,maximum)
  require(stable(staged)==authority['stageRecords'][name],'replay')

def validate_receipts(package,compile_proof,ctx,setup):
 phase='receipts';run=ctx['run'];tooling=ctx['tooling']
 require(type(package) is dict and type(compile_proof) is dict,phase)
 package_keys={'schemaVersion','sourceCommit','sourceTree','sourceSnapshotSha256','productVersion','qualification','installer','compilerSha256','templateSha256','renderedScriptSha256','nativeReceiptSha256','runtimeManifestSha256','installerExecutionQualified'}
 compile_keys={'schemaVersion','status','sourceCommit','sourceTree','sourceSnapshotSha256','runId','runAttempt','pythonVersion','pythonSha256','nsisArchiveSha256','nsisPreparationReceiptSha256','nsisExeSha256','nsisVersion','nsisVersionEvidence','nsisFileVersion','nsisResourceSha256','x64ArtifactId','x64ArtifactDigest','arm64ArtifactId','arm64ArtifactDigest','installerSha256','installerBytes','installerExecuted','setupUploaded','transferRetentionDays','reusedInputs','productBuild','tooling'}
 require(set(package)==package_keys and set(compile_proof)==compile_keys,phase)
 require(re.fullmatch(r'3\.[0-9]{1,2}\.[0-9]{1,3}',compile_proof.get('pythonVersion','')) is not None and digest(compile_proof.get('pythonSha256')),phase)
 require(compile_proof.get('nsisFileVersion') is None or (type(compile_proof['nsisFileVersion']) is str and re.fullmatch(r'[0-9., ]{1,40}',compile_proof['nsisFileVersion']) is not None),phase)
 require(type(compile_proof.get('transferRetentionDays')) is int and compile_proof['transferRetentionDays']==1,phase)
 require(digest(package.get('renderedScriptSha256')) and type(package.get('runtimeManifestSha256')) is dict and set(package['runtimeManifestSha256'])=={'x64','arm64'} and all(digest(v) for v in package['runtimeManifestSha256'].values()),phase)
 _,cargo=read(ROOT/'Cargo.toml',128*1024)
 versions=re.findall(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"$',cargo.decode(),re.M)
 require(len(versions)==1 and package.get('productVersion')==versions[0],phase)
 require(compile_proof.get('schemaVersion')=='migration-stream/windows-desktop-installer-qualification/v1' and compile_proof.get('status')=='COMPILED_PASSIVE_BYTES_ONLY','receipts')
 product={'sourceCommit':f.BASE,'sourceTree':f.TREE,'sourceSnapshotSha256':f.SNAPSHOT,'nativeRunId':str(f.RUN),'runAttempt':1}
 require(type(compile_proof.get('productBuild',{}).get('runAttempt')) is int,phase)
 require(compile_proof.get('productBuild')==product and compile_proof.get('tooling')==tooling and compile_proof.get('reusedInputs') is True,phase)
 require(compile_proof.get('runId')==run and type(compile_proof.get('runAttempt')) is int and compile_proof['runAttempt']==1,phase)
 require(compile_proof.get('installerExecuted') is False and compile_proof.get('setupUploaded') is False,phase)
 for key,value in (('sourceCommit',f.BASE),('sourceTree',f.TREE),('sourceSnapshotSha256',f.SNAPSHOT)):
  require(compile_proof.get(key)==value and package.get(key)==value,phase)
 require(compile_proof.get('nsisArchiveSha256')==n.ARCHIVE_SHA256 and compile_proof.get('nsisExeSha256')==n.MEMBER_PINS[n.COMPILER] and compile_proof.get('nsisResourceSha256')==n.MEMBER_PINS,phase)
 require(compile_proof.get('nsisVersion')==n.VERSION and compile_proof.get('nsisVersionEvidence')=='PINNED_ARCHIVE_RECEIPT' and digest(compile_proof.get('nsisPreparationReceiptSha256')),phase)
 for arch,pin in f.INPUTS.items():
  require(compile_proof.get(arch+'ArtifactId')==str(pin['id']) and compile_proof.get(arch+'ArtifactDigest')==pin['digest'],phase)
 require(package.get('schemaVersion')==p.SCHEMA and package.get('qualification')=='PASSIVE_RECEIPT_BYTE_BINDING_ONLY' and package.get('installerExecutionQualified') is False,phase)
 require(package.get('compilerSha256')==n.MEMBER_PINS[n.COMPILER] and package.get('nativeReceiptSha256')=={arch:pin['receipt'] for arch,pin in f.INPUTS.items()},phase)
 template,_=read(ROOT/'native/windows/desktop-installer.nsi.in',128*1024)
 require(package.get('templateSha256')==template['sha256'],phase)
 require(type(setup['bytes']) is int and 0<setup['bytes']<=p.MAX_SETUP and digest(setup['sha256']),phase)
 require(package.get('installer')=={'bytes':setup['bytes'],'sha256':setup['sha256'],'signingPerformed':False},phase)
 require(compile_proof.get('installerSha256')==setup['sha256'] and type(compile_proof.get('installerBytes')) is int and compile_proof['installerBytes']==setup['bytes'],phase)
 return product

def _publish(stage,target):
 require(os.name=='nt','stage');os.rename(stage,target)

def stage_candidate(compiled,compile_path,output,ctx,*,compile_sha,publish=_publish):
 file_set(compiled,FILES[:2],'stage');p.real_path(output.parent);p.no_streams(output.parent);p.real_path(output,absent=True)
 require(compiled.is_dir() and output.parent.is_dir() and not output.exists() and not p.overlaps(output,compiled),'stage')
 require({x.name for x in compiled.iterdir()}==set(FILES[:2]),'stage')
 records={};payload={}
 for name in FILES:
  source=compile_path if name==FILES[2] else compiled/name
  records[name],payload[name]=read(source,p.MAX_SETUP if name==FILES[0] else 16384)
 p.pe(payload[FILES[0]],0x14c,0x10b)
 package=p.parse_json(payload[FILES[1]]);compile_proof=p.parse_json(payload[FILES[2]])
 require(digest(compile_sha) and records[FILES[2]]['sha256']==compile_sha,'receipts')
 product=validate_receipts(package,compile_proof,ctx,records[FILES[0]])
 authority={'schemaVersion':'migration-stream/windows-engineering-candidate-authority/v1','runId':ctx['run'],'tooling':ctx['tooling'],'productBuild':product,'files':{name:{'bytes':records[name]['bytes'],'sha256':records[name]['sha256']} for name in FILES},'sourceRecords':stable(records),'stageRecords':{}}
 work=Path(tempfile.mkdtemp(prefix='.engineering-candidate-',dir=output.parent));p.no_streams(work)
 for name in FILES:
  with (work/name).open('xb') as stream:stream.write(payload[name])
  copied,_=read(work/name,p.MAX_SETUP if name==FILES[0] else 16384)
  require(all(copied[key]==authority['files'][name][key] for key in ('bytes','sha256')),'stage')
 for name in FILES:
  source=compile_path if name==FILES[2] else compiled/name
  require(read(source,p.MAX_SETUP if name==FILES[0] else 16384)[0]==records[name],'stage')
 publish(work,output)
 file_set(output,FILES,'stage')
 for name in FILES:
  record,_=read(output/name,p.MAX_SETUP if name==FILES[0] else 16384)
  require(all(record[key]==authority['files'][name][key] for key in ('bytes','sha256')),'stage')
  authority['stageRecords'][name]=stable(record)
 return authority

def artifact_admission(inventory,ctx,artifact_id,artifact_digest,now):
 require(type(inventory) is dict and type(inventory.get('total_count')) is int and inventory['total_count']==1 and type(inventory.get('artifacts')) is list and len(inventory['artifacts'])==1,'artifact-metadata')
 value=inventory['artifacts'][0];require(type(value) is dict,'artifact-metadata')
 require(re.fullmatch('[0-9]{1,20}',artifact_id) is not None and digest(artifact_digest),'artifact-metadata')
 require(type(value.get('id')) is int and value.get('id')==int(artifact_id) and type(value.get('size_in_bytes')) is int and 0<value['size_in_bytes']<=p.MAX_SETUP+16*1024**2,'artifact-metadata')
 require(value.get('name')=='migration-stream-engineering-candidate-'+ctx['run'] and value.get('digest')=='sha256:'+artifact_digest and value.get('expired') is False,'artifact-metadata')
 owner=value.get('workflow_run',{})
 require(type(owner.get('id')) is int and owner.get('id')==int(ctx['run']) and owner.get('head_sha')==ctx['tooling']['commit'] and owner.get('head_branch')=='main' and owner.get('repository_id')==1329975731 and owner.get('head_repository_id')==1329975731,'artifact-metadata')
 created=dt.datetime.fromisoformat(value['created_at'].replace('Z','+00:00'));expiry=dt.datetime.fromisoformat(value['expires_at'].replace('Z','+00:00'))
 require(now.tzinfo is not None and created.tzinfo is not None and expiry.tzinfo is not None and created<=now<expiry and abs((expiry-created).total_seconds()-7*86400)<=300,'artifact-metadata')
 return {'artifactId':artifact_id,'artifactDigest':artifact_digest,'artifactBytes':value['size_in_bytes'],'expiresAt':value['expires_at']}

def replay_candidate(download,authority,artifact,ctx):
 file_set(download,FILES,'replay')
 fresh_sources(authority,ctx)
 require(authority.get('runId')==ctx['run'] and authority.get('tooling')==ctx['tooling'] and set(authority.get('files',{}))==set(FILES),'replay')
 payload={}
 for name in FILES:
  record,payload[name]=read(download/name,p.MAX_SETUP if name==FILES[0] else 16384)
  require({key:record[key] for key in ('bytes','sha256')}==authority['files'][name],'replay')
 p.pe(payload[FILES[0]],0x14c,0x10b)
 product=validate_receipts(p.parse_json(payload[FILES[1]]),p.parse_json(payload[FILES[2]]),ctx,authority['files'][FILES[0]])
 require(authority.get('productBuild')==product,'replay')
 fresh_sources(authority,ctx)
 return {'schemaVersion':'migration-stream/windows-engineering-candidate-publication/v1','status':'ENGINEERING_CANDIDATE_PUBLISHED_PASSIVE_REPLAY','setupUploaded':True,'installerExecuted':False,'runId':ctx['run'],'runAttempt':1,'productBuild':product,'tooling':ctx['tooling'],'artifact':artifact,'files':authority['files'],'retentionDays':7,'signingPerformed':False}

def metadata_get(opener,token,run,clock=time.monotonic):
 require(type(token) is str and 1<=len(token)<=4096 and re.fullmatch('[0-9]{1,20}',run) is not None,'artifact-metadata')
 request=urllib.request.Request('https://api.github.com/repos/'+f.REPO+'/actions/runs/'+run+'/artifacts?per_page=2',headers={'Authorization':'Bearer '+token,'Accept':'application/vnd.github+json','X-GitHub-Api-Version':'2022-11-28','User-Agent':'migration-stream-engineering-candidate'})
 deadline=clock()+15
 with opener.open(request,timeout=10) as response:
  require(type(response.status) is int and response.status==200,'artifact-metadata')
  raw=response.read(1024*1024+1);require(len(raw)<=1024*1024 and clock()<deadline,'artifact-metadata')
 return p.parse_json(raw)

def outputs(values):
 with open(os.environ['GITHUB_OUTPUT'],'a',encoding='utf-8') as stream:
  for key,value in values.items():
   require(re.fullmatch('[a-z_]+',key) is not None and re.fullmatch('[A-Za-z0-9._-]{1,128}',str(value)) is not None,'output')
   stream.write(key+'='+str(value)+'\n')

def trusted_json(path,sha):
 require(digest(sha),'receipts');record,value=json_file(path);require(record['sha256']==sha,'receipts');return value

def main():
 try:
  require(sys.argv[1:] in (['--stage'],['--admit-artifact'],['--replay']),'context');ctx=context(dict(os.environ));parent=ctx['parent']
  if sys.argv[1:] == ['--stage']:
   value=stage_candidate(parent/'installer',parent/'qualification-compile.json',parent/'engineering-candidate',ctx,compile_sha=os.environ.get('COMPILE_RECEIPT_SHA'))
   target=parent/'candidate-authority.json'
   with target.open('xb') as stream:stream.write(canonical(value))
   outputs({'candidate_ready':'true','authority_sha':read(target,16384)[0]['sha256']})
   print('{"status":"ENGINEERING_CANDIDATE_STAGED","setupUploaded":false}')
  elif sys.argv[1:] == ['--admit-artifact']:
   p.real_path(parent/'candidate-download',absent=True)
   artifact_id=os.environ.get('CANDIDATE_ARTIFACT_ID','');require(re.fullmatch('[0-9]{1,20}',artifact_id) is not None,'artifact-metadata')
   token=os.environ.get('GH_TOKEN');require(type(token) is str and 1<=len(token)<=4096,'artifact-metadata')
   opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),f.NoRedirect())
   inventory=metadata_get(opener,token,ctx['run'])
   value=artifact_admission(inventory,ctx,artifact_id,os.environ.get('CANDIDATE_ARTIFACT_DIGEST'),dt.datetime.now(dt.timezone.utc))
   target=parent/'candidate-artifact.json'
   with target.open('xb') as stream:stream.write(canonical(value))
   outputs({'artifact_metadata_sha':read(target,16384)[0]['sha256']})
   print('{"status":"ENGINEERING_CANDIDATE_ARTIFACT_ADMITTED"}')
  else:
   authority=trusted_json(parent/'candidate-authority.json',os.environ.get('CANDIDATE_AUTHORITY_SHA'))
   artifact=trusted_json(parent/'candidate-artifact.json',os.environ.get('CANDIDATE_METADATA_SHA'))
   require(artifact.get('artifactId')==os.environ.get('CANDIDATE_ARTIFACT_ID') and artifact.get('artifactDigest')==os.environ.get('CANDIDATE_ARTIFACT_DIGEST'),'replay')
   value=replay_candidate(parent/'candidate-download',authority,artifact,ctx)
   print(json.dumps(value,separators=(',',':')))
  return 0
 except Refused as error:print(json.dumps(error.evidence,separators=(',',':')));return 1
 except Exception:print('{"status":"ENGINEERING_CANDIDATE_REFUSED","phase":"unknown"}');return 1
if __name__=='__main__':raise SystemExit(main())
