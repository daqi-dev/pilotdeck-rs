from pathlib import Path
import sys
p=Path(sys.argv[1])/'meson.build';source=p.read_text()
# Mirrors sharp-libvips v1.2.4's explicit omission of tools/tests/po/fuzz.
for name in ('man','po','tools','test','fuzz'):
 line=f"subdir('{name}')\n"
 assert source.count(line)==1,(name,source.count(line))
 source=source.replace(line,'')
p.write_text(source)
