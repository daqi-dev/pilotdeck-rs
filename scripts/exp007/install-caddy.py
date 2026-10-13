#!/usr/bin/env python3
"""Optional probe dependency, pinned official mac/arm64 archive with SHA512 check."""
import hashlib, pathlib, platform, tarfile, urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[2]
assert platform.system()=='Darwin' and platform.machine()=='arm64', 'This experiment pins mac arm64 only'
name='caddy_2.10.2_mac_arm64.tar.gz'
base='https://github.com/caddyserver/caddy/releases/download/v2.10.2/'
archive=ROOT/'_build/caddy.tar.gz';archive.parent.mkdir(parents=True,exist_ok=True)
checks=urllib.request.urlopen(base+'caddy_2.10.2_checksums.txt').read().decode()
expected=next(line.split()[0] for line in checks.splitlines() if line.endswith(name))
data=urllib.request.urlopen(base+name).read()
assert hashlib.sha512(data).hexdigest()==expected, 'Caddy checksum mismatch'
archive.write_bytes(data)
out=ROOT/'_build/tools/bin';out.mkdir(parents=True,exist_ok=True)
with tarfile.open(archive) as package: package.extract(package.getmember('caddy'),out,filter='data')
print('verified archive SHA256',hashlib.sha256(data).hexdigest())
