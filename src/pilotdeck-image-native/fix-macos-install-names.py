"""Rewrite only this isolated source build's Mach-O library identities."""
import pathlib, subprocess, sys
root=pathlib.Path(sys.argv[1]).resolve()
for lib in sorted(root.glob("*.dylib")):
    if lib.is_symlink() or not lib.is_file():
        continue
    listed=subprocess.run(["otool", "-L", str(lib)], check=True, capture_output=True, text=True).stdout.splitlines()[1:]
    subprocess.run(["install_name_tool", "-id", "@rpath/"+lib.name, str(lib)], check=True)
    for line in listed:
        dep=line.strip().split(" (", 1)[0]
        if dep.startswith(str(root)+"/"):
            subprocess.run(["install_name_tool", "-change", dep, "@rpath/"+pathlib.Path(dep).name, str(lib)], check=True)
