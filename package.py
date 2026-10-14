#!/usr/bin/env python3
# Module: magic_usb.packaging
# Purpose: Build scoped standalone driver packages on their target distribution.
# Architecture: Distro-derived ELF dependencies; non-overlapping installation paths.
from pathlib import Path
import argparse,hashlib,os,shutil,subprocess,tempfile,tomllib
root=Path(__file__).resolve().parent
parser=argparse.ArgumentParser();parser.add_argument('driver',choices=['csr8510','tp-link-ub500']);args=parser.parse_args()
osinfo=dict(line.split('=',1) for line in Path('/etc/os-release').read_text().splitlines() if '=' in line)
ident=osinfo['ID'].strip('"');version=osinfo['VERSION_ID'].strip('"')
target=os.environ.get('PACKAGE_TARGET',ident+version)
assert (ident,version,target) in [('ubuntu','24.04','ubuntu24.04'),('ubuntu','24.04','mint22'),('ubuntu','26.04','ubuntu26.04'),('debian','13','debian13')] or (ident=='linuxmint' and version.startswith('22') and target=='mint22'),'Build inside the target distro/base'
name=args.driver;client='csr8510' if name=='csr8510' else 'ub500'
release=tomllib.loads((root/'clients'/client/'Cargo.toml').read_text())['package']['version']+'-1'+target
arch=subprocess.check_output(['dpkg','--print-architecture'],text=True).strip()
binary=root/'bin'/name;assert binary.is_file()
outputs=root/'outputs';outputs.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='magic-driver-package-') as temp:
 stage=Path(temp);payload=stage/'payload';control=payload/'DEBIAN';control.mkdir(parents=True)
 dest=payload/'usr/libexec'/name/name;dest.parent.mkdir(parents=True);shutil.copy2(binary,dest);dest.chmod(0o755)
 units=payload/'usr/lib/systemd/system';units.mkdir(parents=True)
 unit=(root/'packaging/magic-usb@.service').read_text()
 unit=unit.replace('Description=Magic USB Rust Bluetooth transport','Description='+name+' Rust Bluetooth transport').replace('/usr/bin/magic-usb','/usr/libexec/'+name+'/'+name)
 other='tp-link-ub500' if name=='csr8510' else 'csr8510'
 unit=unit.replace('Conflicts=csr8510@%i.service tp-link-ub500@%i.service','Conflicts=magic-usb@%i.service '+other+'@%i.service')
 (units/(name+'@.service')).write_text(unit)
 doc=payload/'usr/share/doc'/name;doc.mkdir(parents=True)
 for f in ['COPYING','LICENSE-NOTES.md','README.txt','PACKAGING.md']:shutil.copy2(root/f,doc/f)
 debian=stage/'debian';debian.mkdir();(debian/'control').write_text(f'Source: {name}\nSection: net\nPriority: optional\nMaintainer: NullMagic2 <28734029+NullMagic2@users.noreply.github.com>\n\nPackage: {name}\nArchitecture: any\nDescription: Rust userspace Bluetooth driver\n')
 deps=subprocess.check_output(['dpkg-shlibdeps','-O','-e'+str(dest)],cwd=stage,text=True).strip().removeprefix('shlibs:Depends=')
 (control/'control').write_text(f'Package: {name}\nVersion: {release}\nArchitecture: {arch}\nSection: net\nPriority: optional\nMaintainer: NullMagic2 <28734029+NullMagic2@users.noreply.github.com>\nDepends: {deps}, bluez, systemd, kmod\nDescription: Rust userspace Bluetooth transport for {name}\n Scoped USB ownership with BlueZ integration and graceful adapter removal.\n')
 (control/'postinst').write_text(f'''#!/bin/sh
set -e
if [ "$1" = configure ] && [ -d /run/systemd/system ]; then
 systemctl daemon-reload
 systemctl list-units --no-legend --plain --state=active '{name}@*.service' |
 while read -r unit rest; do
  pid=$(systemctl show "$unit" -p MainPID --value)
  if [ "$pid" -gt 0 ] && ! cmp -s "/proc/$pid/exe" /usr/libexec/{name}/{name}; then
   systemctl try-restart "$unit"
  fi
 done
fi
''')
 (control/'prerm').write_text(f'''#!/bin/sh
set -e
case "$1" in remove|deconfigure)
 if [ -d /run/systemd/system ]; then systemctl stop '{name}@*.service'; fi;;
esac
''')
 (control/'postrm').write_text('#!/bin/sh\nset -e\nif [ -d /run/systemd/system ]; then systemctl daemon-reload; fi\n')
 for f in ['postinst','prerm','postrm']:(control/f).chmod(0o755)
 hashes=[]
 for f in sorted((payload/'usr').rglob('*')):
  if f.is_file():hashes.append(hashlib.md5(f.read_bytes()).hexdigest()+'  '+str(f.relative_to(payload)))
 (control/'md5sums').write_text('\n'.join(hashes)+'\n')
 output=outputs/f'{name}_{release}_{arch}.deb'
 subprocess.run(['dpkg-deb','--root-owner-group','--build',str(payload),str(output)],check=True)
 output.with_name(output.name+'.sha256').write_text(hashlib.sha256(output.read_bytes()).hexdigest()+'  '+output.name+'\n')
 print(output)
