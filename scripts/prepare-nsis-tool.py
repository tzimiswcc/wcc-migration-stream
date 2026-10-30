#!/usr/bin/env python3
"""Explicit portable NSIS 3.13 preparation for ephemeral Windows build jobs.

This downloads a fixed official archive only when the CLI is explicitly run.
No installer/compiler/process is executed. No PATH/registry/global config change.
"""
from __future__ import annotations
import argparse
from dataclasses import dataclass
import hashlib
import io
import importlib.util
import json
import os
from pathlib import Path
import re
import ssl
import stat
import struct
import sys
import tempfile
import time
from urllib.parse import unquote,urlsplit
from urllib.request import Request,build_opener,HTTPSHandler,HTTPRedirectHandler,ProxyHandler
import zipfile

VERSION='3.13'
PREFIX='nsis-3.13'
SOURCE_URL='https://downloads.sourceforge.net/project/nsis/NSIS%203/3.13/nsis-3.13.zip'
ARCHIVE_SHA256='ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8'
COMPILER='Bin/makensis.exe'  # Root makensis.exe is a child-spawning SubStart stub.
MEMBER_PINS={
 'Bin/makensis.exe':'65343c225ec88393cc4453b0963caf63f763f42f48cb55736d51893ff2e29d4a',
 'Include/LogicLib.nsh':'2f831300fd6596db53fb9d8e6201818ef92965f5ee10e49dbf96a995cbe3440a',
 'Include/x64.nsh':'aea6ab8bae28e987d4ceac5385b29d781aab5c3a1879f66a4639be1ee7a5d269',
 'Include/FileFunc.nsh':'c144d49f6a5f657aee8366abcdc5798c587a43b9273820797aece1ebc50cefcd',
 'Include/nsDialogs.nsh':'c7d9e29cae063eef051866692699d225e1d33a1919f3d20c39fb4a937de76422',
 'Plugins/x86-unicode/System.dll':'afe032f942e2ed0bf7494ec08ba813f5739fb522c640bde81d30ecaf9f894432',
 'Plugins/x86-unicode/nsDialogs.dll':'9c76f4d53ceee2a471c936e0899688405e1009ce962d140e9acb49ed68d114cb',
 'Stubs/lzma-x86-unicode':'7a37a19706b44313499402a80e1e973be5e64cdf1413b5ed81b3547956a983e0',
 'Stubs/uninst':'ba82bb5d90262417a18cec6631bbd8b880020eb159b45f264a9145196dfb8f3a',
}
MAX_ARCHIVE=16*1024**2
MAX_EXPANDED=16*1024**2
MAX_MEMBER=4*1024**2
MAX_ENTRIES=1024
MAX_RECEIPT=16384
@dataclass(frozen=True)
class _Pins:
 sha256:str
 size:int
 entries:int
 expanded:int
 members:dict
_PINS=_Pins(ARCHIVE_SHA256,2363925,441,7141489,MEMBER_PINS)
class Refused(ValueError):pass

def require(value):
 if not value:raise Refused('portable NSIS preparation refused')
def sha(data):return hashlib.sha256(data).hexdigest()
def canonical(value):return json.dumps(value,sort_keys=True,separators=(',',':')).encode('utf-8')
def signature(info):return (info.st_dev,info.st_ino,info.st_mode,info.st_size,info.st_mtime_ns,info.st_ctime_ns)

def _ordinary(path):
 require(path.is_absolute() and '..' not in path.parts and len(str(path))<=4096 and len(path.parts)<=64)
 if os.name=='nt':
  require(re.match(r'^[A-Za-z]:\\',str(path)) is not None)
  for part in path.parts[1:]:_component(part)
def _component(part):
 require(type(part) is str and 1<=len(part)<=255 and part not in ('.','..') and not part.endswith(('.', ' ')))
 require(not any(c in part for c in '<>:"/\\|?*') and all(c.isprintable() for c in part))
 require(re.fullmatch(r'(?i:CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³])(?:\..*)?',part) is None)
def _plain(path,directory=False,absent=False):
 _ordinary(path)
 for entry in (*reversed(path.parents),path):
  try:info=entry.lstat()
  except FileNotFoundError:
   require(absent and entry==path);return
  require(not stat.S_ISLNK(info.st_mode) and not getattr(info,'st_file_attributes',0)&0x400)
  if entry!=path or directory:require(stat.S_ISDIR(info.st_mode))
  else:require(stat.S_ISREG(info.st_mode) and info.st_nlink==1)
 require(not absent)
_FILE_POLICY=None
def _file_policy():
 global _FILE_POLICY
 if _FILE_POLICY is None:
  spec=importlib.util.spec_from_file_location('nsis_preparation_file_policy',Path(__file__).with_name('package-windows-desktop.py'))
  module=importlib.util.module_from_spec(spec);sys.modules[spec.name]=module;spec.loader.exec_module(module);_FILE_POLICY=module
 return _FILE_POLICY
def _read(path,maximum):
 # Reuse the reviewed Windows lstat/fstat interface binding, binary CRT reads,
 # ADS/nlink/reparse and freshness policy; do not reintroduce Unix-only file IDs.
 return _file_policy().file_record(path,maximum)[1]

def _url_allowed(url):
 parsed=urlsplit(url);host=parsed.hostname or ''
 require(parsed.scheme=='https' and parsed.username is None and parsed.password is None and parsed.port in (None,443) and not parsed.fragment and len(parsed.query)<=4096)
 require(host=='downloads.sourceforge.net' or host.endswith('.dl.sourceforge.net'))
 require(unquote(parsed.path)=='/project/nsis/NSIS 3/3.13/nsis-3.13.zip')
class _Redirect(HTTPRedirectHandler):
 def __init__(self):super().__init__();self.count=0
 def redirect_request(self,req,fp,code,msg,headers,newurl):
  self.count+=1;require(self.count<=3);_url_allowed(newurl)
  return super().redirect_request(req,fp,code,msg,headers,newurl)

def _download(*,opener=None,clock=time.monotonic,pins=_PINS):
 start=clock();_url_allowed(SOURCE_URL)
 if opener is None:opener=build_opener(ProxyHandler({}),HTTPSHandler(context=ssl.create_default_context()),_Redirect())
 request=Request(SOURCE_URL,headers={'User-Agent':'Migration-Stream-NSIS-Preparation/1','Accept-Encoding':'identity'})
 with opener.open(request,timeout=10) as response:
  require(clock()-start<60 and response.status==200);_url_allowed(response.geturl())
  length=response.headers.get('Content-Length')
  require(length is None or re.fullmatch(r'[0-9]{1,8}',length) and int(length)==pins.size)
  require(response.headers.get('Content-Encoding','identity')=='identity')
  data=bytearray()
  while True:
   require(clock()-start<60)
   chunk=response.read(65536);require(clock()-start<60)
   if not chunk:break
   require(len(data)+len(chunk)<=min(MAX_ARCHIVE,pins.size));data.extend(chunk)
  require(len(data)==pins.size and sha(data)==pins.sha256)
  require(clock()-start<60)
  return bytes(data)

def _inspect(data,pins):
 require(len(data)==pins.size and len(data)<=MAX_ARCHIVE and sha(data)==pins.sha256)
 rows={};files={};folded=set();expanded=0
 with zipfile.ZipFile(io.BytesIO(data)) as z:
  entries=z.infolist();require(len(entries)==pins.entries and 1<=len(entries)<=MAX_ENTRIES and not z.comment)
  for item in entries:
   name=item.filename;require(type(name) is str and len(name.encode('utf-8'))<=1024 and not item.flag_bits and item.compress_type==zipfile.ZIP_DEFLATED and not item.extra and not item.comment)
   parts=name.rstrip('/').split('/');require(parts[0]==PREFIX and len(parts)<=16)
   for part in parts:_component(part)
   key=name.rstrip('/').casefold();require(key not in folded);folded.add(key)
   directory=item.is_dir();kind=(item.external_attr>>16)&0xf000
   require(not item.external_attr&0x400 and kind in (0,stat.S_IFDIR if directory else stat.S_IFREG))
   require(0<=item.file_size<=MAX_MEMBER)
   expanded+=item.file_size;require(expanded<=MAX_EXPANDED)
   if directory:require(item.file_size==0);value=None
   else:
    with z.open(item) as source:value=source.read(MAX_MEMBER+1)
    require(len(value)==item.file_size and len(value)<=MAX_MEMBER)
    files['/'.join(parts)]=value
   rows['/'.join(parts)]={'type':'directory' if directory else 'file','bytes':item.file_size,'sha256':None if directory else sha(value)}
 require(expanded==pins.expanded)
 # Every implicit parent becomes an explicit directory in the captured graph.
 for name in list(rows):
  for parent in Path(name).parents:
   if str(parent)=='.':continue
   key=parent.as_posix()
   require(key not in rows or rows[key]['type']=='directory')
   rows.setdefault(key,{'type':'directory','bytes':0,'sha256':None})
 require(len({name.casefold() for name in rows})==len(rows))
 for relative,expected in pins.members.items():
  name=PREFIX+'/'+relative;require(name in files and sha(files[name])==expected)
 compiler=files[PREFIX+'/'+COMPILER]
 require(len(compiler)>=256 and compiler[:2]==b'MZ')
 offset=struct.unpack_from('<I',compiler,60)[0]
 require(64<=offset<=1024**2 and offset+26<=len(compiler) and compiler[offset:offset+4]==b'PE\0\0' and struct.unpack_from('<H',compiler,offset+4)[0]==0x14c and struct.unpack_from('<H',compiler,offset+24)[0]==0x10b)
 return rows,files

def _receipt(rows,pins):
 return {'schemaVersion':'migration-stream/portable-nsis-tool/v1','version':VERSION,'archiveSha256':pins.sha256,'archiveBytes':pins.size,'archiveEntries':pins.entries,'expandedBytes':pins.expanded,'compilerRelativePath':PREFIX+'/'+COMPILER,'compilerSha256':pins.members[COMPILER],'resourceSha256':pins.members,'treeSha256':sha(canonical(rows)),'executableInvokedByPreparation':False}

def _tree(root):
 result={};pending=[root];admitted=0
 while pending:
  parent=pending.pop();_plain(parent,True)
  with os.scandir(parent) as iterator:
   for entry in iterator:
    admitted+=1;require(admitted<=MAX_ENTRIES+16)
    path=Path(entry.path);info=path.lstat();directory=stat.S_ISDIR(info.st_mode);_plain(path,directory)
    name=path.relative_to(root).as_posix()
    result[name]={'type':'directory' if directory else 'file','bytes':0 if directory else info.st_size,'sha256':None if directory else sha(_read(path,MAX_MEMBER))}
    if directory:pending.append(path)
 return result

def _prepare_bytes(data,destination,pins):
 _plain(destination,True,absent=True);rows,files=_inspect(data,pins)
 stage=Path(tempfile.mkdtemp(prefix='.nsis-prepare-',dir=destination.parent))
 # The exact archive and owned partials are retained; no recursive cleanup.
 with (stage/'release.zip').open('xb') as output:output.write(data)
 for name,row in sorted(rows.items(),key=lambda item:(item[0].count('/'),item[0])):
  path=stage/name
  if row['type']=='directory':path.mkdir(mode=0o700)
  else:
   with path.open('xb') as output:output.write(files[name]);output.flush();os.fsync(output.fileno())
 require(_tree(stage/PREFIX)=={name[len(PREFIX)+1:]:row for name,row in rows.items() if name!=PREFIX})
 with (stage/'prepared.json').open('xb') as output:output.write(canonical(_receipt(rows,pins))+b'\n')
 _plain(destination,True,absent=True)
 os.rename(stage,destination)  # Windows refuses an existing destination.
 return _validate_prepared(destination,pins)

def _validate_prepared(destination,pins):
 _plain(destination,True)
 names=[]
 with os.scandir(destination) as iterator:
  for entry in iterator:
   require(len(names)<3);names.append(entry.name)
 require(set(names)=={'release.zip','prepared.json',PREFIX})
 data=_read(destination/'release.zip',MAX_ARCHIVE);rows,_=_inspect(data,pins)
 require(_tree(destination/PREFIX)=={name[len(PREFIX)+1:]:row for name,row in rows.items() if name!=PREFIX})
 raw=_read(destination/'prepared.json',MAX_RECEIPT);require(raw==canonical(_receipt(rows,pins))+b'\n')
 return {**_receipt(rows,pins),'compiler':str(destination/PREFIX/COMPILER),'resourceRoot':str(destination/PREFIX),'preparedDirectory':str(destination),'preparedReceiptSha256':sha(raw)}

def _admit_runner_temp(destination,root):
 _plain(root,True);_ordinary(destination)
 require(destination!=root and root in destination.parents)
def validate_prepared(destination):
 require(os.name=='nt');_admit_runner_temp(destination,Path(os.environ['RUNNER_TEMP']))
 return _validate_prepared(destination,_PINS)
def prepare(destination):
 require(os.name=='nt');_admit_runner_temp(destination,Path(os.environ['RUNNER_TEMP']));_plain(destination,True,absent=True)
 return _prepare_bytes(_download(),destination,_PINS)

if __name__=='__main__':
 parser=argparse.ArgumentParser(description=__doc__)
 selection=parser.add_mutually_exclusive_group(required=True)
 selection.add_argument('--destination',type=Path)
 selection.add_argument('--validate-prepared',type=Path)
 try:
  arguments=parser.parse_args()
  result=prepare(arguments.destination) if arguments.destination is not None else validate_prepared(arguments.validate_prepared)
  print(json.dumps(result,sort_keys=True,separators=(',',':')))
 except (Refused,OSError,ValueError,KeyError,TypeError,zipfile.BadZipFile):
  print('{"status":"REFUSED","stage":"portable-nsis-preparation"}');raise SystemExit(1)
