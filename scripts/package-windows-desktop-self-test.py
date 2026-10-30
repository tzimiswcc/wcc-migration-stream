#!/usr/bin/env python3
"""Offline synthetic paired-layout/NSIS-source tests; never compile or install."""
import contextlib
from dataclasses import replace
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import stat
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest import mock

SPEC=importlib.util.spec_from_file_location("windows_desktop_package",Path(__file__).with_name("package-windows-desktop.py"))
module=importlib.util.module_from_spec(SPEC);sys.modules[SPEC.name]=module
assert SPEC and SPEC.loader
SPEC.loader.exec_module(module)

# Independent literal fixture contract: src/runtime_bundle.rs Windows paths.
# Never construct these from the packager constants being tested.
FIXTURE_EXE_ROLES = {
    "PLATFORM_LAUNCHER": "migration-stream-local.exe",
    "RUNTIME_CONTROLLER": "libexec/migration-stream-local-runtime.exe",
    "INTEGRITY_CLI": "bin/migration-stream.exe",
    "WORKBENCH": "bin/migration-stream-workbench.exe",
}


def fake_pe(machine,magic=0x20B):
    data=bytearray(256);data[:2]=b"MZ";struct.pack_into("<I",data,60,64);data[64:68]=b"PE\0\0";struct.pack_into("<H",data,68,machine);struct.pack_into("<H",data,88,magic);return bytes(data)


class WindowsDesktopSourceTests(unittest.TestCase):
    def setUp(self):
        if getattr(self, "retain_native_fixture", False):
            self.root = Path(tempfile.mkdtemp(prefix="migration-stream-nsis-native-synthetic-")).resolve()
        else:
            self.temp=tempfile.TemporaryDirectory(prefix="migration-stream-installer-synthetic-")
            self.root=Path(self.temp.name).resolve()
        self.commit="b"*40;self.tree="c"*40;self.snapshot="d"*64
        self.inputs={}
        for architecture,(target,machine) in module.TARGETS.items():
            layout=self.root/(architecture+" layout Ω");runtime=layout/"Runtime";runtime.mkdir(parents=True)
            members=[];hashes={}
            for role,name in FIXTURE_EXE_ROLES.items():
                path=runtime/name;path.parent.mkdir(parents=True,exist_ok=True);data=fake_pe(machine);path.write_bytes(data);path.chmod(0o555)
                digest=hashlib.sha256(data).hexdigest();hashes[role]=digest
                members.append({"path":name,"memberType":"REGULAR_FILE","role":role,"mode":"0555","size":len(data),"sha256":digest})
            asset=runtime/"web/index.html";asset.parent.mkdir();asset.write_bytes(b"synthetic frontend");asset.chmod(0o444)
            members.append({"path":"web/index.html","memberType":"REGULAR_FILE","role":"FRONTEND_ASSET","mode":"0444","size":len(asset.read_bytes()),"sha256":hashlib.sha256(asset.read_bytes()).hexdigest()})
            manifest={"schemaVersion":"migration-stream/assisted-local-runtime-bundle/v2","canonicalJsonVersion":"migration-stream-canonical-json/v1","manifestSha256":"0"*64,"product":{"name":"migration-stream","version":"0.1.0","commit":self.commit,"sourceState":"CLEAN","sourceSnapshotSha256":self.snapshot},"target":{"os":"windows","architecture":architecture,"targetTriple":target},"entrypoints":{"launcher":FIXTURE_EXE_ROLES["PLATFORM_LAUNCHER"],"runtimeController":FIXTURE_EXE_ROLES["RUNTIME_CONTROLLER"],"integrityCli":FIXTURE_EXE_ROLES["INTEGRITY_CLI"],"workbench":FIXTURE_EXE_ROLES["WORKBENCH"]},"members":sorted(members,key=lambda row:row["path"])}
            manifest["manifestSha256"]=hashlib.sha256(module.canonical(manifest)).hexdigest()
            raw=module.canonical(manifest);(runtime/"bundle-manifest.json").write_bytes(raw);(runtime/"bundle-manifest.json").chmod(0o444)
            launcher=layout/"migration-stream-document-open.exe";launcher.write_bytes(fake_pe(machine));launcher.chmod(0o555)
            receipt={"schemaVersion":"migration-stream/windows-engineering-qualification/v1","status":"PASSED","stage":"complete","commandExitCode":0,"sourceCommit":self.commit,"sourceTree":self.tree,"sourceSnapshotSha256":self.snapshot,"architecture":architecture,"target":target,"frontendAssetsSha256":"e"*64,"documentLauncherSha256":hashlib.sha256(launcher.read_bytes()).hexdigest(),"runtimeManifestSha256":manifest["manifestSha256"],"runtimeManifestFileSha256":hashlib.sha256(raw).hexdigest(),"runtimeLauncherSha256":hashes["PLATFORM_LAUNCHER"],"runtimeControllerSha256":hashes["RUNTIME_CONTROLLER"],"integrityCliSha256":hashes["INTEGRITY_CLI"],"workbenchSha256":hashes["WORKBENCH"],"runtimeMemberCount":len(members),"runtimeMemberBytes":sum(row["size"] for row in members)}
            receipt.update(runId="123",runAttempt=1,nodeVersion="24.22.1",npmVersion="12.2.1",rustVersion="1.99.1",pythonVersion="3.12.10",engineeringAssemblerSha256="f"*64,toolSha256={name:"a"*64 for name in ["git","rustup","node","python","cargo","rustc","npmShim"]})
            receipt_path=self.root/(architecture+" receipt.json");receipt_path.write_bytes(module.canonical(receipt))
            self.inputs[architecture]=(layout,receipt_path,hashlib.sha256(receipt_path.read_bytes()).hexdigest())
        self.compiler=self.root/"trusted fake makensis.exe";self.compiler.write_bytes(b"FAKE compiler never execute")
        self.request=module.Request(*self.inputs["x64"],*self.inputs["arm64"],self.commit,self.tree,self.snapshot,"0.1.0",self.compiler,hashlib.sha256(self.compiler.read_bytes()).hexdigest(),self.root/"new installer")
        self.compiler_calls=[];self.rendered=""
        self.guard=mock.patch.object(module.subprocess,"Popen",side_effect=AssertionError("no compiler/product/installer process in source tests"));self.guard.start()

    def tearDown(self):
        self.guard.stop()
        for path,_,_ in os.walk(self.root,followlinks=False):os.chmod(path,0o700)
        self.temp.cleanup()

    def compile(self,program,script):
        self.compiler_calls.append((program,script));self.rendered=script.read_text()
        line=next(line for line in self.rendered.splitlines() if line.startswith("OutFile "))
        output=Path(line[len('OutFile "'):-1]);output.write_bytes(fake_pe(0x14C,0x10B))

    def package(self,request=None,compiler=None):return module.package(request or self.request,compiler=compiler or self.compile)

    def validate(self,architecture="x64",request=None):
        request=request or self.request
        return module.validate_layout(getattr(request,architecture+"_layout"),getattr(request,architecture+"_receipt"),getattr(request,architecture+"_receipt_sha256"),architecture,request)

    def mutate_receipt(self,architecture,change):
        layout,path,_=self.inputs[architecture];value=json.loads(path.read_bytes());change(value);path.write_bytes(module.canonical(value));digest=hashlib.sha256(path.read_bytes()).hexdigest();self.inputs[architecture]=(layout,path,digest)
        self.request=replace(self.request,**{architecture+"_receipt_sha256":digest})

    def test_shared_passive_flow_two_payloads_one_setup_and_no_runtime_execution(self):
        self.assertEqual(module.EXE_ROLES,FIXTURE_EXE_ROLES)
        result=self.package();self.assertFalse(result["installerExecutionQualified"])
        self.assertEqual(result["qualification"],"PASSIVE_RECEIPT_BYTE_BINDING_ONLY")
        self.assertEqual(set(path.name for path in self.request.output_directory.iterdir()),{"Migration Stream Setup.exe","engineering-installer.json"})
        self.assertEqual(len(self.compiler_calls),1)
        self.assertIn('${IsNativeAMD64}',self.rendered);self.assertIn('${IsNativeARM64}',self.rendered)
        self.assertIn('Unicode true',self.rendered);self.assertIn('RequestExecutionLevel user',self.rendered)
        self.assertNotIn('@@',self.rendered)
        # NSIS strips quotes only around the entire token, not after /oname=.
        self.assertIn('File "/oname=migration-stream.exe" "',self.rendered)
        self.assertIn('File "/oname=install-owner.txt" "',self.rendered)
        self.assertNotIn('File /oname="',self.rendered)
        self.assertIn('  ClearErrors\n  File "/oname=migration-stream.exe" "',self.rendered)
        self.assertIn('  Push "$INSTDIR\\Runtime\\bin\\migration-stream.exe"\n  Call RequireExtractedFile',self.rendered)
        self.assertIn('  ClearErrors\n  File "/oname=install-owner.txt" "',self.rendered)
        self.assertIn('  Push "$INSTDIR\\install-owner.txt"\n  Call RequireExtractedFile',self.rendered)
        self.assertIn('IfFileExists "$0" 0 extracted_refused',self.rendered)
        self.assertIn('GetFileAttributesW(w r0)i.r1 ?e',self.rendered)
        self.assertIn('IntOp $1 $1 & 0x410',self.rendered)
        self.assertTrue(list(self.root.glob('.migration-stream-nsis-inputs-*')))
        self.assertEqual(module.file_record(self.request.output_directory/"Migration Stream Setup.exe",module.MAX_SETUP)[0]["sha256"],result["installer"]["sha256"])

    def test_required_cli_and_no_skip_or_target_picker_override(self):
        argv=[]
        for name,value in vars(self.request).items():argv.extend(['--'+name.replace('_','-'),str(value)])
        self.assertEqual(module.arguments(argv),self.request)
        for index in range(0,len(argv),2):
            with contextlib.redirect_stderr(io.StringIO()),self.assertRaises(SystemExit):module.arguments(argv[:index]+argv[index+2:])
        with contextlib.redirect_stderr(io.StringIO()),self.assertRaises(SystemExit):module.arguments(argv+['--skip-verify'])
        prepared=self.root/'prepared'
        self.assertEqual(native_compile_directory(['--native-compile-only','--prepared-nsis',str(prepared)]),prepared)
        for invalid in ([],['--native-compile-only'],['--native-compile-only','--prepared-nsis','relative'],['--native-compile-only','--makensis',str(prepared)],['--native-compile-only','--prepared-nsis',str(prepared),'extra']):
            with self.assertRaises(ValueError):native_compile_directory(invalid)

    def test_pair_source_tree_version_snapshot_and_tuple_refusals(self):
        for field,value in [("source_commit","a"*40),("source_tree","a"*40),("source_snapshot_sha256","a"*64),("product_version","9.9.9")]:
            with self.subTest(field=field),self.assertRaises(module.PackagingError):self.validate(request=replace(self.request,**{field:value}))
        self.mutate_receipt("arm64",lambda value:value.update(target="x86_64-pc-windows-msvc"))
        with self.assertRaises(module.PackagingError):self.validate("arm64")

    def test_pinned_receipt_is_external_trust_boundary_not_self_assertion(self):
        with self.assertRaises(module.PackagingError):self.validate(request=replace(self.request,x64_receipt_sha256="0"*64))
        self.mutate_receipt("x64",lambda value:value.update(architecture=["x64","other"]))
        with self.assertRaises(module.PackagingError):self.validate()

    def test_pe_capture_wrong_machine_offsets_signature_magic_and_truncation(self):
        for data in [fake_pe(0xAA64),b"short",b"XX"+fake_pe(0x8664)[2:],fake_pe(0x8664,0x10B)]:
            with self.assertRaises(module.PackagingError):module.pe(data,0x8664)
        layout,_,_=self.inputs['x64'];outer=layout/'migration-stream-document-open.exe';outer.chmod(0o755);outer.write_bytes(fake_pe(0xAA64))
        self.mutate_receipt('x64',lambda value:value.update(documentLauncherSha256=hashlib.sha256(outer.read_bytes()).hexdigest()))
        with self.assertRaises(module.PackagingError):self.validate()
        data=bytearray(fake_pe(0x8664));struct.pack_into("<I",data,60,1048577)
        with self.assertRaises(module.PackagingError):module.pe(data,0x8664)

    def test_member_hash_and_manifest_self_hash_tamper_refuse(self):
        layout,_,_=self.inputs['x64'];file=layout/'Runtime/web/index.html';file.chmod(0o644);file.write_bytes(b"tampered")
        with self.assertRaises(module.PackagingError):self.validate()
        layout,_,_=self.inputs['arm64'];file=layout/'Runtime/bundle-manifest.json';file.chmod(0o644);file.write_bytes(file.read_bytes()+b"\n")
        with self.assertRaises(module.PackagingError):self.validate('arm64')

    def test_unlisted_graph_links_hardlinks_and_special_names_refuse(self):
        layout,_,_=self.inputs['x64'];(layout/'Runtime/extra').write_bytes(b"unexpected")
        with self.assertRaises(module.PackagingError):self.validate()
        (layout/'Runtime/extra').unlink();alias=layout/'Runtime/alias';alias.symlink_to(layout/'Runtime/web/index.html')
        with self.assertRaises(module.PackagingError):self.validate()
        alias.unlink();os.link(layout/'Runtime/web/index.html',alias)
        with self.assertRaises(module.PackagingError):self.validate()
        for name in ['../x','C:ads','CON','x/NUL.txt','x//y','x\\y','x.']:
            with self.subTest(name=name),self.assertRaises(module.PackagingError):module.relative_name(name)

    def test_json_duplicate_float_depth_and_byte_caps(self):
        for data in [b'{"x":1,"X":2}',b'{"size":1.0}',b'['*25+b'0'+b']'*25,b'x'*(module.MAX_JSON+1)]:
            with self.assertRaises((module.PackagingError,ValueError)):module.parse_json(data)
        file=self.root/'tiny';file.write_bytes(b'abc')
        with self.assertRaises(module.PackagingError):module.file_record(file,2)

    def test_nsis_literals_preserve_unicode_spaces_and_refuse_expansion_injection(self):
        self.assertEqual(module.nsis_literal('C:\\synthetic Ω space\\file.exe'),'"C:\\synthetic Ω space\\file.exe"')
        for value in ['${ANY}','x!include malicious','x"\nFile evil','x\x00y','x\u2028y']:
            with self.assertRaises(module.PackagingError):module.nsis_literal(value)
        with self.assertRaises(module.PackagingError):self.package(replace(self.request,output_directory=self.root/'${bad}'))
        self.assertEqual(self.compiler_calls,[])
        self.assertEqual(module.compiler_environment(self.root,self.root),{'SystemRoot':str(self.root),'TEMP':str(self.root),'TMP':str(self.root)})

    def test_no_existing_output_overwrite_or_input_overlap(self):
        self.request.output_directory.mkdir();sentinel=self.request.output_directory/'sentinel';sentinel.write_bytes(b'preserved')
        with self.assertRaises(module.PackagingError):self.package()
        self.assertEqual(sentinel.read_bytes(),b'preserved');self.assertEqual(self.compiler_calls,[])
        with self.assertRaises(module.PackagingError):self.package(replace(self.request,output_directory=self.request.x64_layout/'nested'))

    def test_compiler_failure_preserves_partials_without_public_result(self):
        def fail(_program,_script):raise module.PackagingError('synthetic compiler refusal')
        with self.assertRaises(module.PackagingError):self.package(compiler=fail)
        self.assertFalse(self.request.output_directory.exists());self.assertTrue(list(self.root.glob('.migration-stream-nsis-inputs-*')))

    def test_copied_payload_mutation_during_compilation_refuses(self):
        def changed(program,script):
            self.compile(program,script);path=script.parent/'arm64/Runtime/web/index.html';path.chmod(0o644);path.write_bytes(b'changed copied payload')
        with self.assertRaises(module.PackagingError):self.package(compiler=changed)
        self.assertFalse(self.request.output_directory.exists())

    def test_publication_collision_preserves_foreign_sentinel(self):
        def raced(program,script):
            self.compile(program,script);self.request.output_directory.mkdir();(self.request.output_directory/'sentinel').write_bytes(b'foreign')
        with self.assertRaises(module.PackagingError):self.package(compiler=raced)
        self.assertEqual((self.request.output_directory/'sentinel').read_bytes(),b'foreign')

    def test_installer_policy_openwith_no_default_override_no_lifecycle_or_recursive_uninstall(self):
        self.package();text=self.rendered
        for forbidden in ['RequestExecutionLevel admin','HKLM','UserChoice','ExecShell','cmd.exe','powershell','RMDir /r','Delete "$INSTDIR\\*','/REBOOTOK']:
            self.assertNotIn(forbidden,text)
        self.assertIn('WriteRegNone HKCU "Software\\Classes\\.mstream\\OpenWithProgids"',text)
        self.assertIn('SetOverwrite off',text);self.assertIn('GetFileAttributesW',text);self.assertIn('0x400',text)
        # Literal NSIS grammar oracle: System captures last-error before returning;
        # pop its stack result unconditionally before branching on attributes.
        self.assertIn("System::Call 'kernel32::GetFileAttributesW(w r1)i.r2 ?e'\n    Pop $3\n    ${If} $2 == -1",text)
        self.assertNotIn('kernel32::GetLastError()',text)
        self.assertIn('CreateDirectoryW(w "$INSTDIR"',text)
        self.assertIn('nsDialogs::SelectFileDialog open',text)
        self.assertIn('ExecWait \'"$INSTDIR\\migration-stream-document-open.exe" "$0"\' $1',text)
        self.assertIn('Call un.OwnedInstallation',text);self.assertIn('Call un.RemoveOwnedOpenWith',text)
        self.assertIn('DeleteRegKey /ifempty',text)
        self.assertNotIn('Delete "$DOCUMENTS',text)
        self.assertNotIn('Delete "$LOCALAPPDATA\\migration-stream-runtime',text)

class FileIdentityTests(unittest.TestCase):
    def setUp(self):
        self.guard=mock.patch.object(module.subprocess,"Popen",side_effect=AssertionError("no process in metadata/binary tests"))
        self.guard.start()

    def tearDown(self):
        self.guard.stop()

    @staticmethod
    def record(**changes):
        fields=dict(st_dev=7,st_ino=123456789012345678901234567890,st_size=8,
                    st_mode=stat.S_IFREG|0o666,st_mtime_ns=400,st_ctime_ns=500,
                    st_birthtime_ns=300,st_nlink=1,st_file_attributes=0x20,st_reparse_tag=0)
        fields.update(changes)
        return SimpleNamespace(**fields)

    def test_documented_windows_mode_and_ctime_difference_passes(self):
        named=self.record(st_mode=stat.S_IFREG|0o777,st_ctime_ns=300)
        opened=self.record()
        module.bind_file_interfaces(named,opened,windows=True)
        self.assertNotEqual(module.signature(named),module.signature(opened))
        # Each interface keeps its own full ctime and mode fingerprint.
        self.assertNotEqual(module.signature(named),module.signature(self.record(st_mode=stat.S_IFREG|0o777,st_ctime_ns=301)))
        self.assertNotEqual(module.signature(opened),module.signature(self.record(st_ctime_ns=501)))

    def test_changed_identity_metadata_alias_or_reparse_refuses(self):
        named=self.record(st_mode=stat.S_IFREG|0o777,st_ctime_ns=300)
        for field,value in [("st_dev",8),("st_ino",123456789012345678901234567891),
                            ("st_ino",0),("st_size",9),("st_mtime_ns",401),("st_birthtime_ns",301),
                            ("st_mode",stat.S_IFREG|0o444),("st_mode",stat.S_IFDIR|0o666),
                            ("st_nlink",2),("st_file_attributes",0x21),("st_file_attributes",0x420),
                            ("st_reparse_tag",0xA000000C)]:
            with self.subTest(field=field,value=value),self.assertRaises(module.PackagingError):
                module.bind_file_interfaces(named,self.record(**{field:value}),windows=True)
        for field in ("st_birthtime_ns","st_file_attributes","st_reparse_tag"):
            opened=self.record();delattr(opened,field)
            with self.subTest(missing=field),self.assertRaises(module.PackagingError):
                module.bind_file_interfaces(named,opened,windows=True)

    def test_same_interface_freshness_catches_ctime_mode_and_link_changes(self):
        before=self.record()
        for changes in ({"st_ctime_ns":501},{"st_mode":stat.S_IFREG|0o777},{"st_nlink":2},
                        {"st_birthtime_ns":301},{"st_file_attributes":0x21},{"st_reparse_tag":1}):
            self.assertNotEqual(module.signature(before),module.signature(self.record(**changes)))
        with self.assertRaises(module.PackagingError):
            module.bind_file_interfaces(self.record(),self.record(st_ctime_ns=501),windows=False)

    def test_reader_refuses_path_or_descriptor_ctime_change_within_interface(self):
        with tempfile.TemporaryDirectory(prefix="migration-stream-freshness-synthetic-") as directory:
            path=Path(directory).resolve()/"synthetic.bin";path.write_bytes(b"unchanged bytes")
            original_fstat=module.os.fstat
            calls=0
            def changed_descriptor(fd):
                nonlocal calls
                calls+=1;info=original_fstat(fd)
                if calls==1:return info
                fields={name:getattr(info,name) for name in dir(info) if name.startswith("st_")}
                fields["st_ctime_ns"]+=1
                return SimpleNamespace(**fields)
            with mock.patch.object(module.os,"fstat",changed_descriptor),self.assertRaises(module.PackagingError):
                module.file_record(path)
            original_lstat=Path.lstat
            calls=0
            def changed_path(candidate,*args,**kwargs):
                nonlocal calls
                info=original_lstat(candidate,*args,**kwargs)
                if candidate!=path:return info
                calls+=1
                if calls<3:return info
                fields={name:getattr(info,name) for name in dir(info) if name.startswith("st_")}
                fields["st_ctime_ns"]+=1
                return SimpleNamespace(**fields)
            with mock.patch.object(Path,"lstat",changed_path),self.assertRaises(module.PackagingError):
                module.file_record(path)

    def test_crlf_control_z_and_binary_open_flag_preserve_exact_bytes(self):
        with tempfile.TemporaryDirectory(prefix="migration-stream-binary-synthetic-") as directory:
            path=Path(directory).resolve()/"synthetic.exe"
            data=b"A\r\nB\x1aC\x00\xff\r\n"
            path.write_bytes(data)
            original_open=module.os.open
            seen=[]
            # On Windows use the real CRT flag. Portable source tests inject an
            # otherwise-unused sentinel and remove it only at the OS boundary.
            flag=getattr(os,"O_BINARY",0x40000000)
            def capture(name,flags,*args,**kwargs):
                seen.append(flags)
                return original_open(name,flags if os.name=="nt" else flags & ~flag,*args,**kwargs)
            with mock.patch.object(module.os,"O_BINARY",flag,create=True),mock.patch.object(module.os,"open",capture):
                record,actual=module.file_record(path)
            self.assertTrue(seen and all(flags & flag for flags in seen))
            self.assertEqual(actual,data)
            self.assertEqual(record["bytes"],len(data))
            self.assertEqual(record["sha256"],hashlib.sha256(data).hexdigest())


def native_compile_directory(arguments):
    if len(arguments)!=3 or arguments[:2]!=["--native-compile-only","--prepared-nsis"]:
        raise ValueError("explicit prepared NSIS directory required")
    path=Path(arguments[2])
    if not path.is_absolute():raise ValueError("absolute prepared NSIS directory required")
    return path


def native_compile_fixture(prepared_directory):
    """Compile tiny fake payloads once; never execute the generated installer."""
    if os.name != "nt":
        print(json.dumps({"ok": False, "code": "NATIVE_WINDOWS_REQUIRED"}))
        return 1
    case = None
    initialized = False
    compiler_workspace = None
    def compile_once(program, script):
        nonlocal compiler_workspace
        if compiler_workspace is not None:
            raise AssertionError("only one synthetic compiler invocation is admitted")
        compiler_workspace = script.parent
        module.compile_nsis(program, script)
    try:
        tool_spec=importlib.util.spec_from_file_location("synthetic_compile_prepared_nsis",Path(__file__).with_name("prepare-nsis-tool.py"))
        tool=importlib.util.module_from_spec(tool_spec);sys.modules[tool_spec.name]=tool;tool_spec.loader.exec_module(tool)
        prepared=tool.validate_prepared(prepared_directory)
        compiler=Path(prepared["compiler"])
        compiler_record, _ = module.file_record(compiler)
        if compiler_record["sha256"]!=prepared["compilerSha256"]:raise ValueError("prepared compiler changed")
        case = WindowsDesktopSourceTests("test_shared_passive_flow_two_payloads_one_setup_and_no_runtime_execution")
        case.retain_native_fixture = True
        case.setUp()
        initialized = True
        # Default tests retain their Popen guard. Only this explicit route uses
        # the bounded real compiler with the same tiny synthetic inputs.
        case.guard.stop()
        request = replace(case.request, makensis=compiler,
                          makensis_sha256=compiler_record["sha256"])
        result = module.package(request, compiler=compile_once)
        if tool.validate_prepared(prepared_directory)!=prepared:raise ValueError("prepared resources changed")
        case.assertFalse(result["installerExecutionQualified"])
        case.assertEqual(result["qualification"], "PASSIVE_RECEIPT_BYTE_BINDING_ONLY")
        print(json.dumps({"schemaVersion": "migration-stream/synthetic-nsis-compile/v1",
                          "ok": True, "qualification": "SYNTHETIC_TEMPLATE_COMPILATION_ONLY",
                          "compilerSha256": compiler_record["sha256"],
                          "installer": result["installer"],
                          "installerExecuted": False, "productPayload": False}, sort_keys=True))
        return 0
    except Exception as error:
        # Only compiler logs from this freshly generated fake-input namespace;
        # never product receipts, user files, environment or arbitrary paths.
        if compiler_workspace is not None and str(error) != "owned compiler cleanup unresolved":
            for name in ("compiler.stdout.log", "compiler.stderr.log"):
                try:
                    _, data = module.file_record(compiler_workspace / name, 65536)
                    print(data[-2048:].decode("utf-8", "replace"), file=sys.stderr)
                except (module.PackagingError, OSError):
                    pass  # Unknown diagnostic files are not inspected.
        print(json.dumps({"schemaVersion": "migration-stream/synthetic-nsis-compile/v1",
                          "ok": False, "code": "SYNTHETIC_COMPILATION_FAILED",
                          "errorType": type(error).__name__, "installerExecuted": False}))
        return 1
    finally:
        if initialized:
            case.guard.stop()
        # The native route never recursively removes a compiler-populated tree,
        # including when child cleanup is uncertain. The CI job owns disposal.
        if case is not None and hasattr(case, "root"):
            print(json.dumps({"retainedSyntheticNamespace": str(case.root)}))


if __name__ == '__main__':
    if "--native-compile-only" in sys.argv[1:]:
        try:prepared_directory=native_compile_directory(sys.argv[1:])
        except ValueError:
            print('{"ok":false,"code":"EXPLICIT_PREPARED_NSIS_REQUIRED"}')
            raise SystemExit(1)
        raise SystemExit(native_compile_fixture(prepared_directory))
    unittest.main()
