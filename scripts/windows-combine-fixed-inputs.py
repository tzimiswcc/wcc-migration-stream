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
import urllib.error

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
ALLOWED = frozenset(('scripts/windows-desktop-transfer.py','scripts/qualify-linux-desktop-packaging.py','scripts/qualify-linux-desktop-packaging-self-test.py','scripts/windows-desktop-installer-qualification.ps1','scripts/windows-desktop-transfer-self-test.py','scripts/windows-combine-fixed-inputs.py','scripts/windows-combine-fixed-inputs-self-test.py','.github/workflows/windows-combine-fixed-inputs.yml','.github/workflows/public-source-ci.yml','config/release-corpus.json','docs/WINDOWS-DESKTOP-QUALIFICATION.md'))
FIXED_DIAGNOSTICS = {'scripts/qualify-linux-desktop-packaging.py': '100644 blob 4ad162f705141b5f5f56556191b516ac7e176cb5', 'scripts/qualify-linux-desktop-packaging-self-test.py': '100644 blob 6d2dc046b171f374ee05012378246a7b0ecd2926'}
CORRECTED_TRANSFER_TOOLING = {'scripts/windows-desktop-transfer.py': {'original': '100644 blob 24f941c2413e3ed36f34cc67cc424877ed6b0565', 'corrected': '100644 blob d915e86e654341f6aec630f3a24a6bbc68687b9b'}, 'scripts/windows-desktop-transfer-self-test.py': {'original': '100644 blob c39271ad15fc8b37a6541416d2cbe82c9e3406ef', 'corrected': '100644 blob e2c260996c2a395fdecc84554bac195beeeac45c'}}
PHASES = frozenset(('source-inventory','source-ref','source-dirty','source-delta','run-projection','job-projection','artifact-projection','expiry','api-transport','api-status','api-size','api-json','unknown'))
CATEGORIES = frozenset(('projection','git','deadline','transport','timeout','redirect','http-status','size','json','unknown','token'))
class Refused(ValueError):
 def __init__(self,phase='unknown',category='projection',http_status=None):
  assert phase in PHASES and category in CATEGORIES
  assert http_status is None or (type(http_status) is int and 100<=http_status<=599)
  self.evidence={'status':'FIXED_NATIVE_PAIR_REFUSED','phase':phase,'category':category}
  if http_status is not None:self.evidence['httpStatus']=http_status
  super().__init__('fixed pair refused')

def require(value,phase='unknown',category='projection'):
 if not value:raise Refused(phase,category)

def source_admission(env, head, tree, main, clean, baseline, current):
 require(env.get('GITHUB_EVENT_NAME')=='workflow_dispatch' and env.get('GITHUB_RUN_ATTEMPT')=='1','source-ref')
 require(env.get('GITHUB_REPOSITORY')==REPO and env.get('GITHUB_REF')=='refs/heads/main','source-ref')
 require(env.get('GITHUB_WORKFLOW_REF')==REPO+'/.github/workflows/windows-combine-fixed-inputs.yml@refs/heads/main','source-ref')
 require(env.get('REUSE_FIXED_NATIVE_PAIR')=='true' and head==main==env.get('GITHUB_SHA')==env.get('EXPECTED_SOURCE_COMMIT'),'source-ref')
 require(len(head)==40 and all(c in '0123456789abcdef' for c in head) and len(tree)==40 and all(c in '0123456789abcdef' for c in tree),'source-ref')
 require(clean,'source-dirty')
 require(set(baseline)-set(current)==set(),'source-delta')
 changed={name for name in baseline.keys()|current.keys() if baseline.get(name)!=current.get(name)}
 require(changed<=ALLOWED,'source-delta')
 for name,record in FIXED_DIAGNOSTICS.items():require(current.get(name)==record,'source-delta')
 for name,pins in CORRECTED_TRANSFER_TOOLING.items():require(baseline.get(name)==pins['original'] and current.get(name)==pins['corrected'],'source-delta')
 # All product files and packager/transfer/template MUST remain byte+mode equal.
 for name in ('scripts/package-windows-desktop.py','native/windows/desktop-installer.nsi.in','Cargo.toml'):
  require(name in baseline and baseline[name]==current.get(name),'source-delta')
 return {'commit':head,'tree':tree}

def producer_admission(run, jobs, artifacts, now):
 require(type(run.get('id')) is int and type(run.get('run_attempt')) is int,'run-projection')
 require(run.get('id')==RUN and run.get('head_sha')==BASE and run.get('head_branch')=='main' and run.get('event')=='workflow_dispatch' and run.get('run_attempt')==1 and run.get('status')=='completed' and run.get('conclusion')=='failure','run-projection')
 require(run.get('repository',{}).get('full_name')==REPO,'run-projection')
 rows=jobs.get('jobs');require(type(rows) is list and len(rows)==3 and jobs.get('total_count')==3,'job-projection')
 require(len({row.get('id') for row in rows})==3,'job-projection')
 for arch,pin in INPUTS.items():
  selected=[row for row in rows if row.get('id')==pin['job']];require(len(selected)==1,'job-projection')
  row=selected[0];require(type(row.get('id')) is int and type(row.get('run_id')) is int,'job-projection')
  require(row.get('run_id')==RUN and row.get('name')=='native-'+arch and row.get('status')=='completed' and row.get('conclusion')=='success','job-projection')
  a=artifacts[arch];require(type(a.get('id')) is int and type(a.get('size_in_bytes')) is int,'artifact-projection')
  require(type(a.get('expired')) is bool and not a['expired'],'artifact-projection')
  require(a.get('id')==pin['id'] and a.get('name')=='migration-stream-installer-native-'+arch+'-'+str(RUN) and a.get('size_in_bytes')==pin['bytes'] and a.get('digest')=='sha256:'+pin['digest'] and a.get('expires_at')==pin['expires'],'artifact-projection')
  owner=a.get('workflow_run',{});require(owner.get('id')==RUN and owner.get('repository_id')==1329975731 and owner.get('head_repository_id')==1329975731 and owner.get('head_sha')==BASE and owner.get('head_branch')=='main','artifact-projection')
  expiry=dt.datetime.fromisoformat(pin['expires'].replace('Z','+00:00'))
  require(now.tzinfo is not None and now+dt.timedelta(minutes=20)<expiry,'expiry')
 return {'sourceCommit':BASE,'sourceTree':TREE,'sourceSnapshotSha256':SNAPSHOT,'nativeRunId':str(RUN),'runAttempt':1}

def git(*args):
 try:
  result=subprocess.run(['git',*args],cwd=ROOT,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10,check=True)
  require(len(result.stdout)<=1024*1024,'source-inventory','size')
  return result.stdout.decode('utf-8').strip()
 except Refused:raise
 except Exception:raise Refused('source-inventory','git') from None

def entries(ref):
 try:
  result={}
  for row in git('ls-tree','-r',ref).splitlines():
   record,name=row.split('\t',1);require(name not in result,'source-inventory');result[name]=record
  return result
 except Refused:raise
 except Exception:raise Refused('source-inventory','projection') from None

class NoRedirect(urllib.request.HTTPRedirectHandler):
 def redirect_request(self,*_args,**_kwargs):raise Refused('api-transport','redirect')

ENDPOINTS = ('/actions/runs/'+str(RUN),'/actions/runs/'+str(RUN)+'/jobs?per_page=100',*( '/actions/artifacts/'+str(pin['id']) for pin in INPUTS.values()))

def metadata_get(opener,token,path,deadline,clock=time.monotonic):
 require(path in ENDPOINTS,'api-transport','projection')
 require(type(token) is str and 1<=len(token)<=4096,'api-transport','token')
 require(clock()<deadline,'api-transport','deadline')
 headers={'Accept':'application/vnd.github+json','X-GitHub-Api-Version':'2022-11-28','User-Agent':'migration-stream-fixed-pair-admission','Authorization':'Bearer '+token}
 request=urllib.request.Request('https://api.github.com/repos/'+REPO+path,headers=headers)
 try:
  with opener.open(request,timeout=min(10,deadline-clock())) as response:
   if type(response.status) is not int or response.status!=200:
    code=response.status if type(response.status) is int and 100<=response.status<=599 else None
    raise Refused('api-status','http-status',code)
   raw=response.read(1024*1024+1);require(len(raw)<=1024*1024,'api-size','size')
  require(clock()<deadline,'api-transport','deadline')
 except Refused:raise
 except urllib.error.HTTPError as error:
  code=error.code if type(error.code) is int and 100<=error.code<=599 else None
  error.close()
  raise Refused('api-status','http-status',code) from None
 except (TimeoutError,):raise Refused('api-transport','timeout') from None
 except (urllib.error.URLError,OSError):raise Refused('api-transport','transport') from None
 except Exception:raise Refused('api-transport','unknown') from None
 try:
  def pairs(rows):
   value={}
   for key,item in rows:require(key not in value,'api-json','json');value[key]=item
   return value
  value=json.loads(raw,object_pairs_hook=pairs);require(type(value) is dict,'api-json','json');return value
 except Refused:raise
 except Exception:raise Refused('api-json','json') from None

def admit():
 require(sys.version_info>=(3,11),'source-inventory')
 env=dict(os.environ)
 tooling=source_admission(env,git('rev-parse','HEAD'),git('rev-parse','HEAD^{tree}'),git('rev-parse','refs/remotes/origin/main'),not git('status','--porcelain=v1','--untracked-files=all'),entries(BASE),entries('HEAD'))
 require(git('rev-parse',BASE+'^{tree}')==TREE,'source-inventory')
 opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect())
 deadline=time.monotonic()+60
 def get(path):return metadata_get(opener,env.get('GH_TOKEN'),path,deadline)
 run=get(ENDPOINTS[0]);jobs=get(ENDPOINTS[1])
 artifacts={arch:get('/actions/artifacts/'+str(pin['id'])) for arch,pin in INPUTS.items()}
 product=producer_admission(run,jobs,artifacts,dt.datetime.now(dt.timezone.utc))
 return {'status':'FIXED_NATIVE_PAIR_ADMITTED','tooling':tooling,'productBuild':product,'inputs':INPUTS}

def main():
 try:
  require(sys.argv[1:]==['--admit'],'source-ref');print(json.dumps(admit(),separators=(',',':')));return 0
 except Refused as error:
  print(json.dumps(error.evidence,separators=(',',':')));return 1
 except Exception:
  print('{"status":"FIXED_NATIVE_PAIR_REFUSED","phase":"unknown","category":"unknown"}');return 1

if __name__=='__main__':raise SystemExit(main())
