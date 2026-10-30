#!/usr/bin/env python3
"""Closed admission for one old native pair; no artifact download or execution."""
from __future__ import annotations
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
REPO = 'tzimiswcc/wcc-migration-stream'
BASE = 'a61b0c3b1d9baab5b85818d947da5c2b67cdc892'
TREE = '98fa8ce2dcb4114db7607df62ab3118c850b6c0c'
RUN = 41057681963
SNAPSHOT = '79a91c758f5b440011d8c71c1c1125591ed5b91c9b08eda9045175093bbe916e'
# This job-to-artifact mapping is retained producer-output evidence reviewed
# with the batch receipt, not a linkage inferred from artifact API metadata.
INPUTS = {
 'x64': {'id':12978336770,'job':124069243792,'bytes':31273102,'digest':'b385d5fd4aa7146464c78718a8a27c0546713790b5971642a0b70dd0ae3605dd','archive':'f15c7672614b5014d1e31c11b25c00598e4c65a5c5ca52f095369946f9b250eb','receipt':'7a11ebd8d7d3b7c9b98473a4ec954c4da9aaf53715a0721e52b41439b902d070','expires':'2026-10-31T12:55:37Z'},
 'arm64': {'id':12978508130,'job':124069243939,'bytes':28103762,'digest':'9f6d2f9de222677116bd3863fcda9ca664b6c2240f1de6e7ac01a1e4f1700be2','archive':'1c1a38c7e2d682fe9e7cd9c1531aaa25695d174ad8a7e1e55fbd3c277438845b','receipt':'8ac32182415859da3cf73bf9120bb2865554aaffd8cedf0c6572c3aa5d0f0bc3','expires':'2026-10-31T13:01:27Z'},
}
# Exact four diagnostics paths plus the reviewed lane's six other paths.
ALLOWED = frozenset(('scripts/qualify-linux-desktop-packaging.py','scripts/qualify-linux-desktop-packaging-self-test.py','scripts/windows-desktop-installer-qualification.ps1','scripts/windows-desktop-transfer-self-test.py','scripts/windows-combine-fixed-inputs.py','scripts/windows-combine-fixed-inputs-self-test.py','.github/workflows/windows-combine-fixed-inputs.yml','.github/workflows/public-source-ci.yml','config/release-corpus.json','docs/WINDOWS-DESKTOP-QUALIFICATION.md'))
FIXED_DIAGNOSTICS = {'scripts/qualify-linux-desktop-packaging.py': '100644 blob 4ad162f705141b5f5f56556191b516ac7e176cb5', 'scripts/qualify-linux-desktop-packaging-self-test.py': '100644 blob 6d2dc046b171f374ee05012378246a7b0ecd2926', 'scripts/windows-desktop-transfer-self-test.py': '100644 blob e0da34f5a5649076f6e744977667adae1e96acb0'}
class Refused(ValueError): pass

def require(value):
 if not value: raise Refused('fixed pair refused')

def source_admission(env, head, tree, main, clean, baseline, current):
 require(env.get('GITHUB_EVENT_NAME')=='workflow_dispatch' and env.get('GITHUB_RUN_ATTEMPT')=='1')
 require(env.get('GITHUB_REPOSITORY')==REPO and env.get('GITHUB_REF')=='refs/heads/main')
 require(env.get('GITHUB_WORKFLOW_REF')==REPO+'/.github/workflows/windows-combine-fixed-inputs.yml@refs/heads/main')
 require(env.get('REUSE_FIXED_NATIVE_PAIR')=='true' and head==main==env.get('GITHUB_SHA')==env.get('EXPECTED_SOURCE_COMMIT'))
 require(len(head)==40 and all(c in '0123456789abcdef' for c in head) and len(tree)==40 and all(c in '0123456789abcdef' for c in tree) and clean)
 require(set(baseline)-set(current)==set())
 changed={name for name in baseline.keys()|current.keys() if baseline.get(name)!=current.get(name)}
 require(changed<=ALLOWED)
 for name,record in FIXED_DIAGNOSTICS.items():require(current.get(name)==record)
 # All product files and packager/transfer/template MUST remain byte+mode equal.
 for name in ('scripts/package-windows-desktop.py','scripts/windows-desktop-transfer.py','native/windows/desktop-installer.nsi.in','Cargo.toml'):
  require(name in baseline and baseline[name]==current.get(name))
 return {'commit':head,'tree':tree}

def producer_admission(run, jobs, artifacts, now):
 require(type(run.get('id')) is int and type(run.get('run_attempt')) is int)
 require(run.get('id')==RUN and run.get('head_sha')==BASE and run.get('head_branch')=='main' and run.get('event')=='workflow_dispatch' and run.get('run_attempt')==1 and run.get('status')=='completed' and run.get('conclusion')=='failure')
 require(run.get('repository',{}).get('full_name')==REPO)
 rows=jobs.get('jobs');require(type(rows) is list and len(rows)==3 and jobs.get('total_count')==3)
 require(len({row.get('id') for row in rows})==3)
 for arch,pin in INPUTS.items():
  selected=[row for row in rows if row.get('id')==pin['job']];require(len(selected)==1)
  row=selected[0];require(type(row.get('id')) is int and type(row.get('run_id')) is int)
  require(row.get('run_id')==RUN and row.get('name')=='native-'+arch and row.get('status')=='completed' and row.get('conclusion')=='success')
  a=artifacts[arch];require(type(a.get('id')) is int and type(a.get('size_in_bytes')) is int)
  require(type(a.get('expired')) is bool and not a['expired'])
  require(a.get('id')==pin['id'] and a.get('name')=='migration-stream-installer-native-'+arch+'-'+str(RUN) and a.get('size_in_bytes')==pin['bytes'] and a.get('digest')=='sha256:'+pin['digest'] and a.get('expires_at')==pin['expires'])
  owner=a.get('workflow_run',{});require(owner.get('id')==RUN and owner.get('repository_id')==1329975731 and owner.get('head_repository_id')==1329975731 and owner.get('head_sha')==BASE and owner.get('head_branch')=='main')
  expiry=dt.datetime.fromisoformat(pin['expires'].replace('Z','+00:00'))
  require(now.tzinfo is not None and now+dt.timedelta(minutes=20)<expiry)
 return {'sourceCommit':BASE,'sourceTree':TREE,'sourceSnapshotSha256':SNAPSHOT,'nativeRunId':str(RUN),'runAttempt':1}

def git(*args):
 result=subprocess.run(['git',*args],cwd=ROOT,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10,check=True)
 require(len(result.stdout)<=1024*1024);return result.stdout.decode('utf-8').strip()

def entries(ref):
 result={}
 for row in git('ls-tree','-r',ref).splitlines():
  record,name=row.split('\t',1);require(name not in result);result[name]=record
 return result

class NoRedirect(urllib.request.HTTPRedirectHandler):
 def redirect_request(self,*_args,**_kwargs): raise Refused('metadata redirect refused')

def admit():
 require(sys.version_info>=(3,11))
 env=dict(os.environ)
 tooling=source_admission(env,git('rev-parse','HEAD'),git('rev-parse','HEAD^{tree}'),git('rev-parse','refs/remotes/origin/main'),not git('status','--porcelain=v1','--untracked-files=all'),entries(BASE),entries('HEAD'))
 require(git('rev-parse',BASE+'^{tree}')==TREE)
 opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect())
 deadline=time.monotonic()+60
 def get(path):
  require(time.monotonic()<deadline)
  headers={'Accept':'application/vnd.github+json','X-GitHub-Api-Version':'2022-11-28','User-Agent':'migration-stream-fixed-pair-admission'}
  token=env.get('GH_TOKEN');require(type(token) is str and 1<=len(token)<=4096)
  headers['Authorization']='Bearer '+token
  request=urllib.request.Request('https://api.github.com/repos/'+REPO+path,headers=headers)
  with opener.open(request,timeout=min(10,deadline-time.monotonic())) as response:
   require(response.status==200);raw=response.read(1024*1024+1);require(len(raw)<=1024*1024)
  require(time.monotonic()<deadline)
  def pairs(rows):
   value={}
   for key,item in rows:require(key not in value);value[key]=item
   return value
  value=json.loads(raw,object_pairs_hook=pairs);require(type(value) is dict);return value
 run=get('/actions/runs/'+str(RUN));jobs=get('/actions/runs/'+str(RUN)+'/jobs?per_page=100')
 artifacts={arch:get('/actions/artifacts/'+str(pin['id'])) for arch,pin in INPUTS.items()}
 product=producer_admission(run,jobs,artifacts,dt.datetime.now(dt.timezone.utc))
 return {'status':'FIXED_NATIVE_PAIR_ADMITTED','tooling':tooling,'productBuild':product,'inputs':INPUTS}

if __name__=='__main__':
 try:
  require(sys.argv[1:]==['--admit']);print(json.dumps(admit(),separators=(',',':')))
 except Exception:
  print('{"status":"FIXED_NATIVE_PAIR_REFUSED"}');raise SystemExit(1)
