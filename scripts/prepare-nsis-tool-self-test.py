#!/usr/bin/env python3
"""Pure fake-fetch/hostile-ZIP tests. Never fetch/extract official tools or run one."""
from dataclasses import replace
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import urllib.request
import zipfile

SPEC=importlib.util.spec_from_file_location('portable_nsis_test_subject',Path(__file__).with_name('prepare-nsis-tool.py'))
m=importlib.util.module_from_spec(SPEC);sys.modules[SPEC.name]=m;SPEC.loader.exec_module(m)
ROOT=Path(__file__).resolve().parents[1]
def fake_pe(machine=0x14c):
 d=bytearray(256);d[:2]=b'MZ';struct.pack_into('<I',d,60,64);d[64:68]=b'PE\0\0';struct.pack_into('<H',d,68,machine);struct.pack_into('<H',d,88,0x10b);return bytes(d)
def fixture(changes=None,extra=(),machine=0x14c):
 files={name:b'SYNTHETIC never execute '+name.encode() for name in m.MEMBER_PINS}
 files[m.COMPILER]=fake_pe(machine);files['makensis.exe']=b'FAKE ROOT STUB NEVER EXECUTE'
 if changes:changes(files)
 output=io.BytesIO()
 with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_DEFLATED) as z:
  for name,data in files.items():z.writestr(m.PREFIX+'/'+name,data)
  for name,data,attributes in extra:
   i=zipfile.ZipInfo(name);i.external_attr=attributes;i.compress_type=zipfile.ZIP_DEFLATED;z.writestr(i,data)
 data=output.getvalue();pins=m._Pins(m.sha(data),len(data),len(files)+len(extra),sum(map(len,files.values()))+sum(len(row[1]) for row in extra),{name:m.sha(files[name]) for name in m.MEMBER_PINS if name in files})
 return data,pins
class Response:
 status=200
 def __init__(self,data,*,url=m.SOURCE_URL,headers=None,read_hook=None):self.data=io.BytesIO(data);self.url=url;self.headers={} if headers is None else headers;self.read_hook=read_hook
 def __enter__(self):return self
 def __exit__(self,*args):self.data.close()
 def geturl(self):return self.url
 def read(self,n):
  if self.read_hook:self.read_hook()
  return self.data.read(n)
class Opener:
 def __init__(self,response):self.response=response;self.calls=[]
 def open(self,request,timeout):self.calls.append((request.full_url,timeout));return self.response

class PreparationTests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory(prefix='nsis-source-synthetic-');self.root=Path(self.temp.name).resolve();self.dest=self.root/'prepared';self.data,self.pins=fixture()
  self.process=mock.patch.object(subprocess,'Popen',side_effect=AssertionError('no process in source fixtures'));self.process.start()
  self.network=mock.patch.object(m,'build_opener',side_effect=AssertionError('no network in source fixtures'));self.network.start()
 def tearDown(self):self.network.stop();self.process.stop();self.temp.cleanup()
 def prepare(self):return m._prepare_bytes(self.data,self.dest,self.pins)
 def test_production_pins_and_direct_compiler_are_explicit(self):
  self.assertEqual(m._PINS.size,2363925);self.assertEqual(m._PINS.entries,441);self.assertEqual(m._PINS.expanded,7141489)
  self.assertEqual(m.ARCHIVE_SHA256,'ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8')
  self.assertEqual(m.MEMBER_PINS[m.COMPILER],'65343c225ec88393cc4453b0963caf63f763f42f48cb55736d51893ff2e29d4a')
  self.assertEqual(m.COMPILER,'Bin/makensis.exe');self.assertNotIn('makensis.exe',m.MEMBER_PINS)
 def test_synthetic_fresh_preparation_and_full_passive_revalidation(self):
  result=self.prepare();self.assertEqual(result['compiler'],str(self.dest/m.PREFIX/'Bin/makensis.exe'))
  self.assertEqual(result['resourceRoot'],str(self.dest/m.PREFIX));self.assertFalse(result['executableInvokedByPreparation'])
  self.assertEqual(m._validate_prepared(self.dest,self.pins),result)
  self.assertEqual((self.dest/'release.zip').read_bytes(),self.data)
 def test_unversioned_pe_uses_archive_receipt_not_optional_pe_metadata(self):
  # Fake PE32 optional header has resource-directory RVA/size zero, just as
  # the pinned direct compiler. It must not need an RT_VERSION to prepare.
  result=self.prepare();compiler=self.dest/m.PREFIX/m.COMPILER
  data=compiler.read_bytes();offset=struct.unpack_from('<I',data,60)[0]
  self.assertEqual(struct.unpack_from('<II',data,offset+24+96+2*8),(0,0))
  self.assertEqual(result['version'],'3.13');self.assertEqual(result['compilerSha256'],self.pins.members[m.COMPILER])
  driver=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text()
  self.assertIn("$prepared.version -ceq '3.13'",driver)
  self.assertNotIn('FileMajorPart',driver);self.assertNotIn('FileMinorPart',driver)
  self.assertIn('$nsisFileVersion=$null',driver)
  self.assertIn("nsisVersion=$prepared.version;nsisVersionEvidence='PINNED_ARCHIVE_RECEIPT';nsisFileVersion=$nsisFileVersion",driver)
  self.assertNotIn('--version',driver);self.assertNotIn('/VERSION',driver)
 def test_wrong_archive_hash_refuses_before_staging(self):
  with self.assertRaises(m.Refused):m._prepare_bytes(self.data,self.dest,replace(self.pins,sha256='0'*64))
  self.assertEqual(list(self.root.iterdir()),[])
 def test_traversal_device_ads_absolute_and_invalid_names_refuse(self):
  for name in ('../escape','/absolute',m.PREFIX+'/../escape',m.PREFIX+'/CON.txt',m.PREFIX+'/NUL .txt',m.PREFIX+'/CON .txt',m.PREFIX+'/a:stream',m.PREFIX+'/trail.',m.PREFIX+'/a\\b'):
   with self.subTest(name=name):
    data,pins=fixture(extra=[(name,b'fake',stat.S_IFREG<<16)])
    with self.assertRaises(m.Refused):m._prepare_bytes(data,self.dest,pins)
    self.assertFalse(self.dest.exists())
  m._component('ordinary .txt');m._component('Δοκιμή όνομα.txt')
 def test_case_aliases_and_links_refuse(self):
  for name,attributes in ((m.PREFIX+'/include/LOGICLIB.NSH',stat.S_IFREG<<16),(m.PREFIX+'/link',stat.S_IFLNK<<16)):
   data,pins=fixture(extra=[(name,b'fake',attributes)])
   with self.assertRaises(m.Refused):m._prepare_bytes(data,self.dest,pins)
 def test_header_machine_and_required_resource_hash_bind_exact_bytes(self):
  bad,pins=fixture(machine=0x8664)
  with self.assertRaises(m.Refused):m._prepare_bytes(bad,self.dest,pins)
  with self.assertRaises(m.Refused):m._prepare_bytes(self.data,self.dest,replace(self.pins,members={**self.pins.members,'Stubs/uninst':'0'*64}))
  data,pins=fixture(changes=lambda files:files.pop('Include/x64.nsh'))
  pins=replace(pins,members={**pins.members,'Include/x64.nsh':'0'*64})
  with self.assertRaises(m.Refused):m._prepare_bytes(data,self.dest,pins)
 def test_count_expansion_and_member_caps_refuse(self):
  for pins in (replace(self.pins,entries=self.pins.entries+1),replace(self.pins,expanded=self.pins.expanded+1)):
   with self.assertRaises(m.Refused):m._prepare_bytes(self.data,self.dest,pins)
  with mock.patch.object(m,'MAX_MEMBER',32),self.assertRaises(m.Refused):self.prepare()
  with mock.patch.object(m,'MAX_ENTRIES',2),self.assertRaises(m.Refused):self.prepare()
 def test_no_overwrite_existing_directory_and_dangling_symlink(self):
  self.dest.mkdir();(self.dest/'sentinel').write_bytes(b'keep')
  with self.assertRaises(m.Refused):self.prepare()
  self.assertEqual((self.dest/'sentinel').read_bytes(),b'keep')
  alias=self.root/'alias';alias.symlink_to(self.root/'missing',target_is_directory=True)
  with self.assertRaises(m.Refused):m._prepare_bytes(self.data,alias,self.pins)
 def test_full_tree_and_receipt_tamper_refuse(self):
  self.prepare();receipt=self.dest/'prepared.json';original=receipt.read_bytes();receipt.write_bytes(original+b' ')
  with self.assertRaises(m.Refused):m._validate_prepared(self.dest,self.pins)
  receipt.write_bytes(original);extra=self.dest/m.PREFIX/'unexpected';extra.write_bytes(b'fake')
  with self.assertRaises(m.Refused):m._validate_prepared(self.dest,self.pins)
 def test_compiler_tamper_and_hardlink_refuse(self):
  self.prepare();compiler=self.dest/m.PREFIX/m.COMPILER;original=compiler.read_bytes();compiler.write_bytes(original+b'fake')
  with self.assertRaises(m.Refused):m._validate_prepared(self.dest,self.pins)
  compiler.write_bytes(original);alias=self.root/'hardlink';import os;os.link(compiler,alias)
  with self.assertRaises((m.Refused,m._file_policy().PackagingError)):m._validate_prepared(self.dest,self.pins)
 def test_fake_fetch_uses_exact_url_and_hash_with_no_proxy_search(self):
  opener=Opener(Response(self.data,headers={'Content-Length':str(len(self.data))}))
  self.assertEqual(m._download(opener=opener,pins=self.pins),self.data);self.assertEqual(opener.calls,[(m.SOURCE_URL,10)])
 def test_redirect_host_scheme_path_credentials_and_limit(self):
  for url in ('http://downloads.sourceforge.net/project/nsis/NSIS%203/3.13/nsis-3.13.zip','https://evil.invalid/project/nsis/NSIS%203/3.13/nsis-3.13.zip','https://downloads.sourceforge.net/other.zip','https://user@downloads.sourceforge.net/project/nsis/NSIS%203/3.13/nsis-3.13.zip',m.SOURCE_URL+'#fragment'):
   with self.assertRaises(m.Refused):m._url_allowed(url)
  redirect=m._Redirect();request=urllib.request.Request(m.SOURCE_URL)
  for _ in range(3):self.assertIsNotNone(redirect.redirect_request(request,None,302,'',{},m.SOURCE_URL))
  with self.assertRaises(m.Refused):redirect.redirect_request(request,None,302,'',{},m.SOURCE_URL)
 def test_fake_fetch_hash_length_encoding_cap_and_late_read_refuse(self):
  for response in (Response(b'wrong'),Response(self.data,headers={'Content-Length':'1'}),Response(self.data,headers={'Content-Encoding':'gzip'}),Response(self.data+b'overflow')):
   with self.assertRaises(m.Refused):m._download(opener=Opener(response),pins=self.pins)
  clock=[0.0];response=Response(self.data,read_hook=lambda:clock.__setitem__(0,61.0))
  with self.assertRaises(m.Refused):m._download(opener=Opener(response),clock=lambda:clock[0],pins=self.pins)
 def test_public_preparation_is_restricted_to_runner_temporary_descendants(self):
  m._admit_runner_temp(self.root/'child',self.root)
  with self.assertRaises(m.Refused):m._admit_runner_temp(self.root,self.root)
  with self.assertRaises(m.Refused):m._admit_runner_temp(self.root.parent/'outside',self.root)
 def test_shared_callers_require_prepared_contract_no_installed_fallback(self):
  ci=(ROOT/'.github/workflows/public-source-ci.yml').read_text();driver=(ROOT/'scripts/windows-desktop-installer-qualification.ps1').read_text();harness=(ROOT/'scripts/package-windows-desktop-self-test.py').read_text()
  self.assertIn('scripts/prepare-nsis-tool.py',ci);self.assertIn('--prepared-nsis',ci);self.assertIn('scripts/prepare-nsis-tool.py',driver)
  self.assertNotIn('NSIS/makensis.exe',driver);self.assertNotIn("'NSIS\\makensis.exe'",ci)
  self.assertIn('validate_prepared',harness);self.assertIn('prepared_directory',harness)
  self.assertNotIn('Path(os.environ["ProgramFiles(x86)"])',harness)

if __name__=='__main__':unittest.main()
