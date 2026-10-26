import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import re

RECIPE = Path(__file__).resolve().parent
PROJECT = RECIPE.parent
BUILD = Path(os.environ.get("SPOTIYA_AIRPLAY_BUILD", Path.home()/".cache/spotiya/airplay2-armv7-glibc223"))
TOOL = Path(os.environ.get("SPOTIYA_TOOLCHAIN", Path.home()/".cache/spotiya/gcc-linaro-6.5.0-2018.12-x86_64_arm-linux-gnueabihf"))
DIST = PROJECT / "dist"
DIST.mkdir(exist_ok=True)

with tempfile.TemporaryDirectory(prefix="airplay-package-",dir=DIST) as scratch:
    root = Path(scratch)/"spotiya-airplay2"
    payload = root/"airplay"
    (payload/"bin").mkdir(parents=True)
    files = {
        "shairport-sync": "shairport-sync/shairport-sync",
        "nqptp": "nqptp/nqptp",
        "avahi-daemon": "avahi/avahi-daemon/avahi-daemon",
        "dbus-daemon": "dbus/bus/dbus-daemon",
    }
    sums = []
    hashes = {}
    for name, source in files.items():
        target = payload/"bin"/name
        shutil.copy2(BUILD/source, target)
        subprocess.run([str(TOOL/"bin/arm-linux-gnueabihf-strip"),"--strip-unneeded",str(target)],check=True)
        versions = subprocess.check_output([str(TOOL/"bin/arm-linux-gnueabihf-readelf"), "--version-info", str(target)], text=True)
        for version in set(re.findall(r"GLIBC_([0-9.]+)", versions)):
            if tuple(map(int, version.split('.'))) > (2, 23):
                raise RuntimeError(f"{name} requires glibc {version}; maximum is 2.23")
        hashes["bin/"+name] = hashlib.sha256(target.read_bytes()).hexdigest()
        sums.append(hashes["bin/"+name]+"  bin/"+name)
    version = tomllib.loads((PROJECT/"Cargo.toml").read_text())["package"]["version"]
    (payload/"manifest.json").write_text(json.dumps({"format": 1, "target": "armv7-unknown-linux-gnueabihf", "spotiya_version": version, "files": hashes}, indent=2)+"\n")
    (payload/"SHA256SUMS").write_text("\n".join(sums)+"\n")
    shutil.copy2(RECIPE/"sources.json",payload/"sources.json")
    for patch in ["avahi-no-reverse.patch", "avahi-runtime-user.patch", "shairport-control.patch", "spotiya-control.c", "spotiya-control.h"]:
        shutil.copy2(RECIPE/patch,payload/patch)
    import fetch
    revisions = dict(fetch.PROJECTS)
    revisions["spotiya_identity"] = {"model": "AudioAccessory5,1", "purpose": "HomePod mini icon; existing AirPlay capabilities retained"}
    (payload/"revisions.json").write_text(json.dumps(revisions,indent=2)+"\n")
    for name in [*fetch.PROJECTS, *[s["name"] for s in json.loads((RECIPE/"sources.json").read_text())]]:
        for source in (BUILD/name).iterdir():
            if source.is_file() and source.name.upper().startswith(("COPYING","LICENSE","LICENCE")):
                destination = payload/"licenses"/name
                destination.mkdir(parents=True,exist_ok=True)
                shutil.copy2(source,destination/source.name)
    archive = DIST/"spotiya-airplay2-armv7.tar.gz"
    with tarfile.open(archive,"w:gz",compresslevel=9) as tar:
        tar.add(root,arcname=root.name)
    (DIST/"AIRPLAY_SHA256SUMS").write_text(hashlib.sha256(archive.read_bytes()).hexdigest()+"  "+archive.name+"\n")
    print(f"AirPlay 2 package: {archive} ({archive.stat().st_size} bytes)")
