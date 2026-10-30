#!/usr/bin/env python3
"""Build one unsigned native-selecting NSIS installer from trusted paired layouts.

Passive byte/PE/graph binding to externally pinned native receipts, never a
replacement Runtime verifier or proof of native lifecycle/installer acceptance.
"""
from __future__ import annotations
import argparse
import ctypes
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import tempfile
import time

MAX_FILE = 128 * 1024 * 1024
MAX_LAYOUT = 642 * 1024 * 1024
MAX_SETUP = 1536 * 1024 * 1024
MAX_JSON = 1024 * 1024
MAX_SCRIPT = 8 * 1024 * 1024
SCHEMA = "migration-stream/windows-desktop-package/v1"
TEMPLATE = Path(__file__).resolve().parents[1] / "native/windows/desktop-installer.nsi.in"
EXE_ROLES = {"PLATFORM_LAUNCHER":"migration-stream-local.exe", "RUNTIME_CONTROLLER":"libexec/migration-stream-local-runtime.exe", "INTEGRITY_CLI":"bin/migration-stream.exe", "WORKBENCH":"bin/migration-stream-workbench.exe"}
ROLES = set(EXE_ROLES) | {"FRONTEND_ASSET", "POLICY", "PROFILE_SCHEMA", "MAPPING_SCHEMA", "BUNDLE_SCHEMA", "CONTROLLED_PACK", "OPERATOR_GUIDE"}
TARGETS = {"x64":("x86_64-pc-windows-msvc",0x8664), "arm64":("aarch64-pc-windows-msvc",0xAA64)}

class PackagingError(ValueError):
    pass

def require(condition):
    if not condition: raise PackagingError("desktop packaging input refused")

def signature(info):
    # A fingerprint is compared only within the same stat interface. CPython's
    # Windows path and descriptor APIs intentionally synthesize different mode
    # and ctime values; neither field is discarded from its own freshness cut.
    return (info.st_dev,info.st_ino,info.st_size,info.st_mode,info.st_mtime_ns,info.st_ctime_ns,
            info.st_nlink,getattr(info,"st_birthtime_ns",None),
            getattr(info,"st_file_attributes",None),getattr(info,"st_reparse_tag",None))

def bind_file_interfaces(named,opened, *, windows=None):
    if windows is None: windows=os.name=="nt"
    for info in (named,opened):
        require(stat.S_ISREG(info.st_mode) and info.st_nlink==1 and info.st_ino!=0)
        require(not (getattr(info,"st_file_attributes",0)&0x400) and not getattr(info,"st_reparse_tag",0))
    if not windows:
        require(signature(named)==signature(opened))
        return
    # CPython 3.12 update_st_mode_from_path adds 0111 by extension; fstat
    # does not. Keep readonly/write bits and all other mode/type bits bound.
    require((named.st_mode & ~0o111)==(opened.st_mode & ~0o111))
    require((named.st_dev,named.st_ino,named.st_size,named.st_mtime_ns,named.st_nlink)==
            (opened.st_dev,opened.st_ino,opened.st_size,opened.st_mtime_ns,opened.st_nlink))
    for field in ("st_birthtime_ns","st_file_attributes","st_reparse_tag"):
        left,right=getattr(named,field,None),getattr(opened,field,None)
        require(left==right)  # Missing on only one interface is unknown/refusal.
        if field=="st_file_attributes": require(left is not None)
    # Path ctime is legacy creation time in CPython 3.12; descriptor ctime is
    # FileBasicInfo.ChangeTime. Each remains checked against its own end read.

def no_streams(path):
    if os.name != "nt": return  # Portable synthetic files have no NTFS stream API.
    from ctypes import wintypes as w
    class StreamData(ctypes.Structure):
        _fields_ = [("size",ctypes.c_longlong),("name",w.WCHAR * 296)]
    api=ctypes.WinDLL("kernel32",use_last_error=True)
    api.FindFirstStreamW.argtypes=[w.LPCWSTR,w.DWORD,ctypes.POINTER(StreamData),w.DWORD]
    api.FindFirstStreamW.restype=w.HANDLE
    api.FindNextStreamW.argtypes=[w.HANDLE,ctypes.POINTER(StreamData)]
    api.FindNextStreamW.restype=w.BOOL
    api.FindClose.argtypes=[w.HANDLE];api.FindClose.restype=w.BOOL
    data=StreamData();ctypes.set_last_error(0)
    handle=api.FindFirstStreamW(str(path),0,ctypes.byref(data),0)
    if handle == ctypes.c_void_p(-1).value:
        require(ctypes.get_last_error()==38)  # Unknown/unsupported is refusal.
        return
    try:
        require(data.name=="::$DATA")
        ctypes.set_last_error(0)
        if api.FindNextStreamW(handle,ctypes.byref(data)):
            raise PackagingError("alternate data stream refused")
        require(ctypes.get_last_error()==38)
    finally:
        api.FindClose(handle)

def real_path(path, *, absent=False):
    require(path.is_absolute() and ".." not in path.parts)
    if os.name=="nt":
        require(re.match(r"^[A-Za-z]:\\",str(path)) is not None)
        for part in path.parts[1:]:
            require(not part.endswith(("."," ")) and not any(char in part for char in ':<>"|?*') and all(char.isprintable() for char in part))
            require(re.fullmatch(r"(?i:CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³])(?:\..*)?",part.split('.')[0].rstrip(' ')) is None)
    for part in [*reversed(path.parents),path]:
        try: info=part.lstat()
        except FileNotFoundError:
            require(absent and part==path);return
        require(not stat.S_ISLNK(info.st_mode) and not (getattr(info,"st_file_attributes",0)&0x400))
        if part!=path: require(stat.S_ISDIR(info.st_mode))
    require(not absent)

def overlaps(first,second):
    for candidate in (second,*second.parents):
        if first.exists() and candidate.exists() and os.path.samefile(first,candidate):return True
    for candidate in (first,*first.parents):
        if second.exists() and candidate.exists() and os.path.samefile(second,candidate):return True
    return False

def file_record(path, maximum=MAX_FILE):
    real_path(path);info=path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink==1 and 0<=info.st_size<=maximum)
    no_streams(path)
    flags=os.O_RDONLY | getattr(os,"O_NOFOLLOW",0) | getattr(os,"O_NONBLOCK",0) | getattr(os,"O_BINARY",0)
    fd=os.open(path,flags)
    try:
        before=os.fstat(fd);bind_file_interfaces(info,before)
        data=bytearray()
        while chunk:=os.read(fd,65536):
            require(len(data)+len(chunk)<=maximum);data.extend(chunk)
        require(len(data)==before.st_size and signature(before)==signature(os.fstat(fd)))
        no_streams(path)
        current=path.lstat()
        require(signature(info)==signature(current))
        bind_file_interfaces(current,before)
        return {"bytes":len(data),"sha256":hashlib.sha256(data).hexdigest(),"mode":stat.S_IMODE(before.st_mode),"identity":signature(before)},bytes(data)
    finally: os.close(fd)

def canonical(value):
    return json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(",",":"),allow_nan=False).encode()

def parse_json(data):
    require(len(data)<=MAX_JSON)
    def pairs(items):
        result={};folded=set()
        for key,value in items:
            require(type(key) is str and key.casefold() not in folded)
            folded.add(key.casefold());result[key]=value
        return result
    def refuse(_value): raise PackagingError("noninteger JSON number refused")
    value=json.loads(data.decode("utf-8"),object_pairs_hook=pairs,parse_float=refuse,parse_constant=refuse)
    count=0
    def walk(item,depth):
        nonlocal count
        count+=1;require(depth<=24 and count<=20000)
        if isinstance(item,dict):
            for child in item.values():walk(child,depth+1)
        elif isinstance(item,list):
            for child in item:walk(child,depth+1)
    walk(value,0);return value

def digest(value):
    require(type(value) is str and re.fullmatch(r"[0-9a-f]{64}",value) is not None);return value

def relative_name(value):
    require(type(value) is str and 0<len(value.encode())<=512)
    parts=value.split("/");require(len(parts)<=64)
    for part in parts:
        require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*",part) is not None and not part.endswith("."))
        require(re.fullmatch(r"(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?",part) is None)
    return value

def pe(data, machine, magic=0x20B):
    require(len(data)>=256 and data[:2]==b"MZ")
    offset=struct.unpack_from("<I",data,60)[0]
    require(64<=offset<=1024*1024 and offset+26<=len(data))
    require(data[offset:offset+4]==b"PE\0\0" and struct.unpack_from("<H",data,offset+4)[0]==machine and struct.unpack_from("<H",data,offset+24)[0]==magic)

def snapshot(root):
    real_path(root);require(stat.S_ISDIR(root.lstat().st_mode));rows={};pending=[(root,"",0)];admitted=1;total=0
    while pending:
        path,name,depth=pending.pop();require(depth<=64)
        real_path(path);info=path.lstat();no_streams(path)
        if stat.S_ISDIR(info.st_mode):
            rows[name]={"type":"directory","mode":stat.S_IMODE(info.st_mode),"identity":signature(info)}
            children=[]
            with os.scandir(path) as entries:
                for entry in entries:
                    require(admitted<4096);admitted+=1
                    child=name+"/"+entry.name if name else entry.name
                    relative_name(child);children.append((Path(entry.path),child,depth+1))
            pending.extend(reversed(sorted(children,key=lambda item:item[1])))
        else:
            record,_=file_record(path);total+=record["bytes"];require(total<=MAX_LAYOUT)
            rows[name]={"type":"file",**record}
    require(set(name for name in rows if name and "/" not in name)=={"Runtime","migration-stream-document-open.exe"})
    return dict(sorted(rows.items()))

@dataclass(frozen=True)
class Request:
    x64_layout:Path
    x64_receipt:Path
    x64_receipt_sha256:str
    arm64_layout:Path
    arm64_receipt:Path
    arm64_receipt_sha256:str
    source_commit:str
    source_tree:str
    source_snapshot_sha256:str
    product_version:str
    makensis:Path
    makensis_sha256:str
    output_directory:Path


def validate_layout(root, receipt_path, receipt_sha, architecture, request):
    receipt_record,raw=file_record(receipt_path,MAX_JSON);require(receipt_record["sha256"]==digest(receipt_sha));receipt=parse_json(raw)
    triple,machine=TARGETS[architecture]
    require(type(receipt) is dict and receipt.get("schemaVersion")=="migration-stream/windows-engineering-qualification/v1" and receipt.get("status")=="PASSED" and receipt.get("stage")=="complete" and type(receipt.get("commandExitCode")) is int and receipt["commandExitCode"]==0)
    require(type(receipt.get("runId")) is str and re.fullmatch(r"[0-9]{1,20}",receipt["runId"]) is not None and type(receipt.get("runAttempt")) is int and receipt["runAttempt"]==1)
    require(receipt.get("nodeVersion")=="24.22.1" and receipt.get("npmVersion")=="12.2.1" and receipt.get("rustVersion")=="1.99.1")
    require(type(receipt.get("pythonVersion")) is str and re.fullmatch(r"3\.[0-9]{1,2}\.[0-9]{1,3}",receipt["pythonVersion"]) is not None)
    require(type(receipt.get("toolSha256")) is dict and set(receipt["toolSha256"])=={"git","rustup","node","python","cargo","rustc","npmShim"})
    for value in receipt["toolSha256"].values():digest(value)
    digest(receipt.get("engineeringAssemblerSha256"))
    require(receipt.get("sourceCommit")==request.source_commit and receipt.get("sourceTree")==request.source_tree and receipt.get("sourceSnapshotSha256")==request.source_snapshot_sha256)
    require(type(receipt.get("architecture")) is str and receipt["architecture"]==architecture and type(receipt.get("target")) is str and receipt["target"]==triple)
    for key in ("frontendAssetsSha256","documentLauncherSha256","runtimeManifestSha256","runtimeManifestFileSha256","runtimeLauncherSha256","runtimeControllerSha256","integrityCliSha256","workbenchSha256"):
        digest(receipt.get(key))
    rows=snapshot(root)
    manifest_record,raw=file_record(root/"Runtime/bundle-manifest.json",MAX_JSON)
    require(manifest_record["sha256"]==receipt["runtimeManifestFileSha256"])
    manifest=parse_json(raw);require(type(manifest) is dict and set(manifest)=={"schemaVersion","canonicalJsonVersion","manifestSha256","product","target","entrypoints","members"})
    require(raw==canonical(manifest) and manifest["schemaVersion"]=="migration-stream/assisted-local-runtime-bundle/v2" and manifest["canonicalJsonVersion"]=="migration-stream-canonical-json/v1")
    digest(manifest["manifestSha256"]);normalized={**manifest,"manifestSha256":"0"*64}
    require(hashlib.sha256(canonical(normalized)).hexdigest()==manifest["manifestSha256"]==receipt["runtimeManifestSha256"])
    product=manifest["product"]
    require(type(product) is dict and set(product)=={"name","version","commit","sourceState","sourceSnapshotSha256"} and product=={"name":"migration-stream","version":request.product_version,"commit":request.source_commit,"sourceState":"CLEAN","sourceSnapshotSha256":request.source_snapshot_sha256})
    require(manifest["target"]=={"os":"windows","architecture":architecture,"targetTriple":triple})
    require(manifest["entrypoints"]=={"launcher":EXE_ROLES["PLATFORM_LAUNCHER"],"runtimeController":EXE_ROLES["RUNTIME_CONTROLLER"],"integrityCli":EXE_ROLES["INTEGRITY_CLI"],"workbench":EXE_ROLES["WORKBENCH"]})
    members=manifest["members"];require(type(members) is list and 1<=len(members)<=1024)
    files={"Runtime/bundle-manifest.json","migration-stream-document-open.exe"};directories={"","Runtime"};seen=set();previous="";total=0;roles={}
    for member in members:
        require(type(member) is dict and set(member)=={"path","memberType","role","mode","size","sha256"})
        name=relative_name(member["path"]);role=member["role"]
        require(name>previous and name.casefold() not in seen and name!="bundle-manifest.json");previous=name;seen.add(name.casefold())
        require(type(role) is str and role in ROLES and member["memberType"]=="REGULAR_FILE")
        require(type(member["size"]) is int and 0<=member["size"]<=(MAX_FILE if role in EXE_ROLES else 8*1024*1024))
        require(member["mode"]==("0555" if role in EXE_ROLES else "0444"));digest(member["sha256"])
        rel="Runtime/"+name;require(rel in rows and rows[rel]["type"]=="file" and rows[rel]["bytes"]==member["size"] and rows[rel]["sha256"]==member["sha256"])
        if os.name!="nt":require(rows[rel]["mode"]==int(member["mode"],8))
        total+=member["size"];require(total<=512*1024*1024);files.add(rel)
        parent=Path(rel).parent
        while parent.as_posix()!=".":directories.add(parent.as_posix());parent=parent.parent
        roles.setdefault(role,[]).append(name)
        if role in EXE_ROLES:
            require(name==EXE_ROLES[role]);record,data=file_record(root/rel);pe(data,machine)
            field={"PLATFORM_LAUNCHER":"runtimeLauncherSha256","RUNTIME_CONTROLLER":"runtimeControllerSha256","INTEGRITY_CLI":"integrityCliSha256","WORKBENCH":"workbenchSha256"}[role]
            require(record["sha256"]==receipt[field])
    require(all(roles.get(role)==[path] for role,path in EXE_ROLES.items()))
    require(len(roles.get("FRONTEND_ASSET",[]))<=256 and sum(member["size"] for member in members if member["role"]=="FRONTEND_ASSET")<=32*1024*1024)
    require(receipt["runtimeLauncherSha256"]==receipt["runtimeControllerSha256"])
    require(set(name for name,row in rows.items() if row["type"]=="file")==files and set(name for name,row in rows.items() if row["type"]=="directory")==directories)
    require(type(receipt.get("runtimeMemberCount")) is int and receipt["runtimeMemberCount"]==len(members) and type(receipt.get("runtimeMemberBytes")) is int and receipt["runtimeMemberBytes"]==total)
    launcher,data=file_record(root/"migration-stream-document-open.exe");pe(data,machine);require(launcher["sha256"]==receipt["documentLauncherSha256"])
    require(snapshot(root)==rows)
    return {"architecture":architecture,"target":triple,"rows":rows,"receiptSha256":receipt_record["sha256"],"receiptIdentity":receipt_record,"manifestSha256":manifest["manifestSha256"],"frontendAssetsSha256":receipt["frontendAssetsSha256"],"documentLauncherSha256":launcher["sha256"]}


def nsis_literal(value):
    # Closed compiler-input subset: Unicode/spaces retained; no preprocessor or
    # variable-expansion syntax, quotes or control characters admitted.
    require(type(value) is str and len(value)<=900 and not any(char in value for char in '$!"') and all(char.isprintable() for char in value))
    return '"'+value+'"'


def compiler_environment(system_directory, temporary_directory):
    root=Path(system_directory);real_path(root);require(stat.S_ISDIR(root.lstat().st_mode))
    real_path(temporary_directory);require(stat.S_ISDIR(temporary_directory.lstat().st_mode))
    return {"SystemRoot":str(root),"TEMP":str(temporary_directory),"TMP":str(temporary_directory)}


def compile_nsis(program, script):
    if os.name!="nt": raise PackagingError("real compiler execution requires Windows")
    import msvcrt
    from ctypes import wintypes as w
    api=ctypes.WinDLL("kernel32",use_last_error=True)
    api.PeekNamedPipe.argtypes=[w.HANDLE,w.LPVOID,w.DWORD,ctypes.POINTER(w.DWORD),ctypes.POINTER(w.DWORD),ctypes.POINTER(w.DWORD)];api.PeekNamedPipe.restype=w.BOOL
    api.GetSystemWindowsDirectoryW.argtypes=[w.LPWSTR,w.UINT];api.GetSystemWindowsDirectoryW.restype=w.UINT
    buffer=ctypes.create_unicode_buffer(32768)
    length=api.GetSystemWindowsDirectoryW(buffer,len(buffer));require(0<length<len(buffer))
    environment=compiler_environment(buffer.value,script.parent)
    deadline=time.monotonic()+300
    logs=[]
    try:
        for name in ("compiler.stdout.log","compiler.stderr.log"):logs.append((script.parent/name).open("xb"))
        child=subprocess.Popen([str(program),"/NOCONFIG","/V2","/INPUTCHARSET","UTF8",str(script)],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=environment,shell=False,bufsize=0)
    except BaseException:
        for output in logs:output.close()
        raise
    counts=[0,0]
    try:
        while True:
            require(time.monotonic()<deadline);read_any=False
            for index,stream in enumerate((child.stdout,child.stderr)):
                available=w.DWORD();ctypes.set_last_error(0)
                ok=api.PeekNamedPipe(msvcrt.get_osfhandle(stream.fileno()),None,0,None,ctypes.byref(available),None)
                if not ok:require(ctypes.get_last_error()==109)
                elif available.value:
                    data=os.read(stream.fileno(),min(4096,available.value));read_any=True
                    logs[index].write(data[:max(0,65536-counts[index])]);counts[index]+=len(data);require(counts[index]<=65536)
                require(time.monotonic()<deadline)
            status=child.poll();require(time.monotonic()<deadline)
            if status is not None and not read_any:
                require(status==0);break
            time.sleep(0.02)
    finally:
        try:
            if child.poll() is None:
                child.terminate()
                try:child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    try:child.wait(timeout=2)
                    except subprocess.TimeoutExpired:raise PackagingError("owned compiler cleanup unresolved") from None
        finally:
            child.stdout.close();child.stderr.close()
            for output in logs:output.close()


def render_files(layout, payload):
    rows=layout["rows"];lines=[]
    for name,row in rows.items():
        if row["type"]!="file":continue
        parent=str(Path(name).parent).replace("/","\\")
        directory="$INSTDIR"+("\\"+parent if parent!="." else "")
        target="$INSTDIR\\"+name.replace("/","\\")
        lines.extend([f'  Push "{directory}"',"  Call RequireSafeAncestors",f'  SetOutPath "{directory}"',f'  IfFileExists "{target}" 0 +2','    Abort "Destination file already exists."','  ClearErrors',f'  File {nsis_literal("/oname="+Path(name).name)} {nsis_literal(str(payload/name))}',"  IfErrors 0 +2",'    Abort "Native payload extraction failed; partial files were preserved."',f'  Push "{target}"','  Call RequireExtractedFile'])
    lines.extend(['  SetOutPath "$INSTDIR"','  IfFileExists "$INSTDIR\\install-owner.txt" 0 +2','    Abort "Existing ownership marker was preserved."','  ClearErrors',f'  File "/oname=install-owner.txt" {nsis_literal(str(payload.parent/(layout["architecture"]+"-owner.txt")))}','  IfErrors 0 +2','    Abort "Ownership marker extraction failed."','  Push "$INSTDIR\\install-owner.txt"','  Call RequireExtractedFile'])
    return "\n".join(lines)


def render_deletes(layout):
    lines=[]
    for name,row in reversed(list(layout["rows"].items())):
        if not name:continue
        target="$INSTDIR\\"+name.replace("/","\\")
        operation="Delete" if row["type"]=="file" else "RMDir"
        lines.extend([f'  Push "{target}"',"  Call un.RequireSafeAncestors",f"  System::Call 'kernel32::GetFileAttributesW(w \"{target}\")i.r0'",'  ${If} $0 != -1',"    ClearErrors",f'    {operation} "{target}"',"    IfErrors 0 +2",'      Abort "Some owned files remain; partial uninstall was preserved."','  ${EndIf}'])
    return "\n".join(lines)


def copy_payload(source,destination,rows):
    destination.mkdir(mode=0o700)
    for name,row in rows.items():
        if not name:continue
        target=destination/name
        if row["type"]=="directory":target.mkdir(mode=0o700)
        else:
            record,data=file_record(source/name);require(record=={key:value for key,value in row.items() if key!="type"})
            with target.open("xb") as output:output.write(data)
            target.chmod(row["mode"])
            actual,_=file_record(target);require((actual["bytes"],actual["sha256"])==(row["bytes"],row["sha256"]))
    for name,row in reversed(list(rows.items())):
        if row["type"]=="directory":(destination/name if name else destination).chmod(row["mode"])


def package(request, *, compiler=compile_nsis):
    require(re.fullmatch(r"[0-9a-f]{40}",request.source_commit) is not None and re.fullmatch(r"[0-9a-f]{40}",request.source_tree) is not None)
    digest(request.source_snapshot_sha256);require(re.fullmatch(r"[0-9]{1,4}\.[0-9]{1,4}\.[0-9]{1,4}",request.product_version) is not None)
    paths=[request.x64_layout,request.arm64_layout,request.x64_receipt,request.arm64_receipt,request.makensis,request.output_directory]
    for index,path in enumerate(paths):
        nsis_literal(str(path));real_path(path,absent=index==5)
        for earlier in paths[:index]:require(not overlaps(path,earlier))
    compiler_record,_=file_record(request.makensis);require(compiler_record["sha256"]==digest(request.makensis_sha256))
    x64=validate_layout(request.x64_layout,request.x64_receipt,request.x64_receipt_sha256,"x64",request)
    arm=validate_layout(request.arm64_layout,request.arm64_receipt,request.arm64_receipt_sha256,"arm64",request)
    require(x64["frontendAssetsSha256"]==arm["frontendAssetsSha256"])
    stage=Path(tempfile.mkdtemp(prefix=".migration-stream-nsis-",dir=request.output_directory.parent))
    work=Path(tempfile.mkdtemp(prefix=".migration-stream-nsis-inputs-",dir=request.output_directory.parent))
    copied={}
    owner="migration-stream:"+request.source_commit+":"+request.source_tree+":"+request.source_snapshot_sha256
    for layout,source in [(x64,request.x64_layout),(arm,request.arm64_layout)]:
        copy_payload(source,work/layout["architecture"],layout["rows"])
        copied[layout["architecture"]]=snapshot(work/layout["architecture"])
        expected={name:{key:value for key,value in row.items() if key!="identity"} for name,row in layout["rows"].items()}
        require({name:{key:value for key,value in row.items() if key!="identity"} for name,row in copied[layout["architecture"]].items()}==expected)
        with (work/(layout["architecture"]+"-owner.txt")).open("x",encoding="ascii",newline="") as output:output.write(owner+":"+layout["architecture"]+"\r\n")
    template_record,template_bytes=file_record(TEMPLATE,128*1024)
    script=template_bytes.decode("utf-8")
    substitutions={"OUTPUT":nsis_literal(str(stage/"Migration Stream Setup.exe")),"VERSION":request.product_version,"OWNER":owner,"FILES_X64":render_files(x64,work/"x64"),"FILES_ARM64":render_files(arm,work/"arm64"),"DELETE_X64":render_deletes(x64),"DELETE_ARM64":render_deletes(arm)}
    for key,value in substitutions.items():require(script.count("@@"+key+"@@")==1);script=script.replace("@@"+key+"@@",value)
    require("@@" not in script and len(script.encode())<=MAX_SCRIPT)
    source_script=work/"installer.nsi"
    with source_script.open("x",encoding="utf-8",newline="\n") as output:output.write(script)
    script_record,_=file_record(source_script,MAX_SCRIPT)
    compiler(request.makensis,source_script)
    require(file_record(TEMPLATE,128*1024)[0]==template_record and file_record(source_script,MAX_SCRIPT)[0]==script_record)
    require(file_record(request.makensis)[0]==compiler_record)
    require(snapshot(request.x64_layout)==x64["rows"] and snapshot(request.arm64_layout)==arm["rows"])
    require(all(snapshot(work/architecture)==rows for architecture,rows in copied.items()))
    require(file_record(request.x64_receipt,MAX_JSON)[0]==x64["receiptIdentity"] and file_record(request.arm64_receipt,MAX_JSON)[0]==arm["receiptIdentity"])
    setup,data=file_record(stage/"Migration Stream Setup.exe",MAX_SETUP);pe(data,0x14C,0x10B)
    receipt={"schemaVersion":SCHEMA,"sourceCommit":request.source_commit,"sourceTree":request.source_tree,"sourceSnapshotSha256":request.source_snapshot_sha256,"productVersion":request.product_version,"qualification":"PASSIVE_RECEIPT_BYTE_BINDING_ONLY","installer":{"bytes":setup["bytes"],"sha256":setup["sha256"],"signingPerformed":False},"compilerSha256":compiler_record["sha256"],"templateSha256":template_record["sha256"],"renderedScriptSha256":script_record["sha256"],"nativeReceiptSha256":{"x64":x64["receiptSha256"],"arm64":arm["receiptSha256"]},"runtimeManifestSha256":{"x64":x64["manifestSha256"],"arm64":arm["manifestSha256"]},"installerExecutionQualified":False}
    # Keep private compiler inputs on both success and failure; never recursively
    # erase a compiler-populated namespace or include it in the output package.
    with (stage/"engineering-installer.json").open("xb") as output:output.write(canonical(receipt)+b"\n")
    # Create-only directory publication. Windows os.rename refuses an existing
    # destination; no POSIX replacement fallback is used by the production CLI.
    if os.name!="nt" and compiler is compile_nsis:raise PackagingError("real publication requires Windows")
    require(not request.output_directory.exists() and not request.output_directory.is_symlink())
    os.rename(stage,request.output_directory)
    final,_=file_record(request.output_directory/"Migration Stream Setup.exe",MAX_SETUP);require(final["sha256"]==setup["sha256"] and final["bytes"]==setup["bytes"])
    return receipt


def arguments(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    for arch in ("x64","arm64"):
        parser.add_argument("--"+arch+"-layout",required=True,type=Path)
        parser.add_argument("--"+arch+"-receipt",required=True,type=Path)
        parser.add_argument("--"+arch+"-receipt-sha256",required=True)
    for name in ("source-commit","source-tree","source-snapshot-sha256","product-version","makensis-sha256"):parser.add_argument("--"+name,required=True)
    for name in ("makensis","output-directory"):parser.add_argument("--"+name,required=True,type=Path)
    return Request(**vars(parser.parse_args(argv)))

if __name__=="__main__":
    try:
        result=package(arguments());print(json.dumps({"schemaVersion":SCHEMA,"ok":True,"installer":result["installer"]},separators=(",",":")))
    except (PackagingError,OSError,ValueError,TypeError,KeyError,RecursionError):
        print(json.dumps({"schemaVersion":SCHEMA,"ok":False,"code":"PACKAGING_REFUSED"},separators=(",",":")));raise SystemExit(1)
