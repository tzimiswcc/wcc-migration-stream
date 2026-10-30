#!/usr/bin/env python3
"""Bounded passive unpack of the exact native engineering ZIP transfer format.

No executable/Runtime verification is performed here. The caller must obtain
both hashes from the admitted native job, then run the separate paired-layout
packager. This is not an archive format for arbitrary inputs or user exports.
"""
from __future__ import annotations
import argparse
import functools
import importlib.util
import sys
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import tempfile
import zipfile

MAX_ARCHIVE=1024**3
MAX_TOTAL=642*1024**2
MAX_MEMBER=128*1024**2
MAX_RECEIPT=1024**2
MAX_ENTRIES=4097
TARGETS={'x64':'x86_64-pc-windows-msvc','arm64':'aarch64-pc-windows-msvc'}
PHASES=frozenset(('paths','receipt-read','receipt-json','build-receipt','archive-read','zip-structure','preview-receipt','extraction','publication','passive-replay','unknown'))
CATEGORIES=frozenset(('guard','identity','freshness','io','format','marked','unknown'))
class Refusal(ValueError):
    def __init__(self,phase='unknown',category='guard'):
        assert phase in PHASES and category in CATEGORIES
        self.evidence={'status':'REFUSED','phase':phase,'category':category}
        super().__init__('engineering transfer refused')

def stage(phase):
    def decorate(function):
        @functools.wraps(function)
        def wrapped(*args,**kwargs):
            try:return function(*args,**kwargs)
            except Refusal as error:
                if error.evidence['phase']=='unknown':raise Refusal(phase,error.evidence['category']) from None
                raise
            except OSError:raise Refusal(phase,'io') from None
            except (ValueError,KeyError,TypeError,RecursionError,zipfile.BadZipFile):raise Refusal(phase,'format') from None
        return wrapped
    return decorate

# Reuse the reviewed interface binder without changing packager/product bytes.
_identity_spec=importlib.util.spec_from_file_location('desktop_transfer_identity',Path(__file__).with_name('package-windows-desktop.py'))
_identity=importlib.util.module_from_spec(_identity_spec);sys.modules[_identity_spec.name]=_identity;_identity_spec.loader.exec_module(_identity)

def require(value):
    if not value: raise Refusal()

def digest(value):
    require(type(value) is str and re.fullmatch('[0-9a-f]{64}',value) is not None)
    return value

@stage('paths')
def plain(path, directory=False, absent=False):
    require(path.is_absolute() and '..' not in path.parts)
    if os.name=='nt':
        require(re.match(r'^[A-Za-z]:\\',str(path)) is not None)
        for part in path.parts[1:]:
            require(not part.endswith(('.', ' ')) and not any(c in part for c in ':<>\"|?*') and all(c.isprintable() for c in part))
            require(re.fullmatch(r'(?i:CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³])(?:\..*)?',part) is None)
    for parent in (*reversed(path.parents),path):
        try:info=parent.lstat()
        except FileNotFoundError:
            require(absent and parent==path);return
        require(not stat.S_ISLNK(info.st_mode) and not getattr(info,'st_file_attributes',0)&0x400)
        if parent!=path or directory:require(stat.S_ISDIR(info.st_mode))
        else:require(stat.S_ISREG(info.st_mode) and info.st_nlink==1)
    require(not absent)

signature=_identity.signature

def bind_file_interfaces(named,opened,phase):
    try:_identity.bind_file_interfaces(named,opened)
    except _identity.PackagingError:raise Refusal(phase,'identity') from None

@stage('receipt-read')
def read_small(path, maximum):
    plain(path);info=path.lstat();require(0<=info.st_size<=maximum)
    with path.open('rb') as source:
        before=os.fstat(source.fileno());bind_file_interfaces(info,before,'receipt-read')
        data=source.read(maximum+1)
        require(len(data)==info.st_size)
        if signature(os.fstat(source.fileno()))!=signature(before):raise Refusal('receipt-read','freshness')
        plain(path);current=path.lstat()
        if signature(current)!=signature(info):raise Refusal('receipt-read','freshness')
        bind_file_interfaces(current,before,'receipt-read')
    return data

@stage('receipt-json')
def json_value(data):
    require(len(data)<=MAX_RECEIPT)
    def pairs(rows):
        result={};seen=set()
        for key,value in rows:
            require(key.casefold() not in seen);seen.add(key.casefold());result[key]=value
        return result
    def reject(_):raise Refusal('receipt-json','format')
    value=json.loads(data.decode('utf-8'),object_pairs_hook=pairs,parse_float=reject,parse_constant=reject)
    count=0
    def visit(node,depth):
        nonlocal count
        count+=1;require(depth<=24 and count<=20000)
        if type(node) is dict:
            for child in node.values():visit(child,depth+1)
        elif type(node) is list:
            for child in node:visit(child,depth+1)
        else:require(node is None or type(node) in (str,int,bool))
    visit(value,0);require(type(value) is dict);return value

def name_ok(name):
    require(type(name) is str and 1<=len(name.encode('ascii'))<=1024 and '\\' not in name)
    pieces=name.rstrip('/').split('/');require(len(pieces)<=66)
    for part in pieces:
        require(re.fullmatch('[A-Za-z0-9][A-Za-z0-9._-]*',part) is not None and not part.endswith('.'))
        require(re.fullmatch(r'(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?',part) is None)
    require(name=='preview-receipt.json' or name.startswith('engineering-layout/'))


@stage('zip-structure')
def structure(source):
    # Mirror the documented existing stored ZIP subset, rejecting ZIP64,
    # descriptors, encryption, extras, preambles, comments and local/central drift.
    size=os.fstat(source.fileno()).st_size;require(22<=size<=MAX_ARCHIVE)
    def read(offset,length):
        require(0<=offset<=size and 0<=length<=8*1024**2 and offset+length<=size)
        source.seek(offset);data=source.read(length);require(len(data)==length);return data
    end=read(size-22,22);require(end[:4]==b'PK\x05\x06')
    disk,central_disk,count_disk,count,central_size,central_offset,comment=struct.unpack_from('<4H2IH',end,4)
    require(disk==central_disk==comment==0 and count_disk==count and 4<=count<=MAX_ENTRIES and central_size<=8*1024**2 and central_offset+central_size==size-22)
    central=read(central_offset,central_size);position=0;next_local=0;total=0;seen=set();names=[]
    for _ in range(count):
        require(position+46<=len(central) and central[position:position+4]==b'PK\x01\x02')
        flags,method=struct.unpack_from('<HH',central,position+8)
        crc,compressed,length=struct.unpack_from('<III',central,position+16)
        n,extra,comment,disk=struct.unpack_from('<4H',central,position+28)
        attributes,local=struct.unpack_from('<II',central,position+38)
        require(flags&0xf7ff==0 and method==0 and compressed==length and length<=MAX_MEMBER and extra==comment==disk==0 and 1<=n<=1024 and position+46+n<=len(central))
        raw=central[position+46:position+46+n];require(all(32<=c<=126 for c in raw));name=raw.decode('ascii');name_ok(name)
        key=name.rstrip('/').casefold();require(key not in seen);seen.add(key)
        kind=(attributes>>16)&0xf000;directory=name.endswith('/')
        require(not attributes&0x400 and kind in (0,0x4000 if directory else 0x8000))
        if directory:require(length==0)
        if name=='preview-receipt.json':require(length<=MAX_RECEIPT)
        total+=length;require(total<=MAX_TOTAL and local==next_local)
        header=read(local,30);require(header[:4]==b'PK\x03\x04' and struct.unpack_from('<HH',header,6)==(flags,method))
        require(struct.unpack_from('<IIIHH',header,14)==(crc,compressed,length,n,0) and read(local+30,n)==raw)
        next_local=local+30+n+length;require(next_local<=central_offset)
        names.append(name);position+=46+n
    require(position==len(central) and next_local==central_offset);source.seek(0);return names


@stage('passive-replay')
def unpack(archive,archive_sha,receipt,receipt_sha,destination,architecture,source_commit,source_tree):
    digest(archive_sha);digest(receipt_sha);require(architecture in TARGETS)
    require(re.fullmatch('[0-9a-f]{40}',source_commit) is not None and re.fullmatch('[0-9a-f]{40}',source_tree) is not None)
    plain(archive);plain(receipt);plain(destination.parent,True);plain(destination,True,absent=True)
    if os.name=='nt':
        # Presence only, never read/remove/rewrite browser zone information.
        for marked in (archive,receipt):
            try: zone=open(str(marked)+':Zone.Identifier','rb')
            except FileNotFoundError: pass
            else: zone.close();raise Refusal('paths','marked')
    require(not destination.exists() and not destination.is_symlink())
    # Dest is new; reject any lexical/physical containing input relationship.
    require(destination not in archive.parents and destination not in receipt.parents)
    raw=read_small(receipt,MAX_RECEIPT);require(hashlib.sha256(raw).hexdigest()==receipt_sha)
    build=json_value(raw)
    require(build.get('schemaVersion')=='migration-stream/windows-engineering-qualification/v1' and build.get('status')=='PASSED' and build.get('stage')=='complete' and type(build.get('commandExitCode')) is int and build['commandExitCode']==0 and type(build.get('runAttempt')) is int and build['runAttempt']==1)
    require(build.get('sourceCommit')==source_commit and build.get('sourceTree')==source_tree and build.get('architecture')==architecture and build.get('target')==TARGETS[architecture]);digest(build.get('sourceSnapshotSha256'))
    info=archive.lstat()
    with archive.open('rb') as source:
        before=os.fstat(source.fileno());bind_file_interfaces(info,before,'archive-read')
        total=0;h=hashlib.sha256()
        while data:=source.read(65536):total+=len(data);require(total<=MAX_ARCHIVE);h.update(data)
        require(h.hexdigest()==archive_sha);names=structure(source)
        with zipfile.ZipFile(source) as z:
            require(z.namelist()==names)
            value=json_value(z.read('preview-receipt.json'))
            require(set(value)=={'schemaVersion','build','runtime','entries'} and value['schemaVersion']=='migration-stream/windows-engineering-preview/v1' and type(value['build']) is dict and value['build']=={key:build[key] for key in ('schemaVersion','status','sourceCommit','sourceTree','architecture','target','sourceSnapshotSha256','frontendAssetsSha256','documentLauncherSha256','engineeringAssemblerSha256','runtimeManifestSha256','runtimeManifestFileSha256','runtimeLauncherSha256','runtimeControllerSha256','integrityCliSha256','workbenchSha256','runtimeMemberCount','runtimeMemberBytes')})
            target=value['runtime'].get('target',{})
            require(target=={'os':'windows','architecture':architecture,'targetTriple':TARGETS[architecture]})
            rows=value['entries'];require(type(rows) is list and 3<=len(rows)<=MAX_ENTRIES-1)
            expected={};folded=set()
            for row in rows:
                require(type(row) is dict and set(row)=={'path','type','bytes','sha256'})
                path=row['path'];require(type(path) is str and row['type'] in ('file','directory') and type(row['bytes']) is int and 0<=row['bytes']<=MAX_MEMBER)
                name='engineering-layout/'+path
                if row['type']=='directory' and not name.endswith('/'):name+='/'
                name_ok(name)
                require(name not in expected and name.rstrip('/').casefold() not in folded);folded.add(name.rstrip('/').casefold())
                if row['type']=='file':digest(row['sha256'])
                else:require(row['bytes']==0 and row['sha256'] is None)
                expected[name]=row
            require(set(names)==set(expected)|{'preview-receipt.json'})
            stage=Path(tempfile.mkdtemp(prefix='.desktop-transfer-',dir=destination.parent))
            # Only our fresh stage is written; no extraction API follows paths.
            for name in names:
                if name=='preview-receipt.json':continue
                row=expected[name];target_path=stage/name.rstrip('/')
                if row['type']=='directory':target_path.mkdir()
                else:
                    require(z.getinfo(name).file_size==row['bytes']);h=hashlib.sha256();count=0
                    with z.open(name) as data, target_path.open('xb') as output:
                        while chunk:=data.read(65536):count+=len(chunk);require(count<=row['bytes']);h.update(chunk);output.write(chunk)
                    require(count==row['bytes'] and h.hexdigest()==row['sha256'])
            if signature(os.fstat(source.fileno()))!=signature(before):raise Refusal('archive-read','freshness')
            plain(archive);current=archive.lstat()
            if signature(current)!=signature(info):raise Refusal('archive-read','freshness')
            bind_file_interfaces(current,before,'archive-read')
            require(read_small(receipt,MAX_RECEIPT)==raw)
            # Windows rename is create-only. POSIX is used only by tiny synthetic
            # tests and is not a Windows publication/atomicity qualification.
            require(not destination.exists() and not destination.is_symlink());os.rename(stage,destination)
    return {'status':'PASSIVE_TRANSFER_ONLY','architecture':architecture,'archiveSha256':archive_sha,'receiptSha256':receipt_sha,'sourceCommit':source_commit,'sourceTree':source_tree,'sourceSnapshotSha256':build['sourceSnapshotSha256']}

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for key in ('archive','receipt','destination'):p.add_argument('--'+key,type=Path,required=True)
    for key in ('archive-sha','receipt-sha','architecture','source-commit','source-tree'):p.add_argument('--'+key,required=True)
    try:print(json.dumps(unpack(**vars(p.parse_args())),separators=(',',':')))
    except Refusal as error:
        print(json.dumps(error.evidence,separators=(',',':')));raise SystemExit(1)
    except Exception:
        print('{"status":"REFUSED","phase":"unknown","category":"unknown"}');raise SystemExit(1)
