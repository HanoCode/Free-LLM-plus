#!/usr/bin/env python3
"""把 electron-<ver>-<platform>.zip 解到 node_modules/electron/dist，保留符号链接。

用法：python3 scripts/unzip-electron.py [zip 路径]
不传参时自动找 electron-cache/*.zip。
幂等：已存在的符号链接/文件会被覆盖，不依赖 rm -rf。
"""

import glob
import os
import stat
import sys
import zipfile

APP = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(APP, "node_modules", "electron", "dist")


def install(zip_path):
    z = zipfile.ZipFile(zip_path)
    n_link = n_file = 0
    for info in z.infolist():
        name = info.filename
        if name.startswith("/") or ".." in name.split("/"):
            continue  # 防 zip-slip
        target = os.path.join(DIST, *name.split("/"))
        mode = (info.external_attr >> 16) & 0xFFFF

        if stat.S_ISLNK(mode):
            dest = z.read(name).decode()
            os.makedirs(os.path.dirname(target), exist_ok=True)
            if os.path.islink(target) or os.path.exists(target):
                os.remove(target)
            os.symlink(dest, target)
            n_link += 1
        elif name.endswith("/"):
            os.makedirs(target, exist_ok=True)
        else:
            os.makedirs(os.path.dirname(target), exist_ok=True)
            if os.path.islink(target):
                os.remove(target)
            with open(target, "wb") as f:
                f.write(z.read(name))
            perm = (mode & 0o777) or 0o644
            os.chmod(target, perm)
            n_file += 1
    z.close()

    os.makedirs(os.path.join(APP, "node_modules", "electron"), exist_ok=True)
    with open(os.path.join(APP, "node_modules", "electron", "path.txt"), "w") as f:
        f.write("Electron.app/Contents/MacOS/Electron")
    print(f"installed: {n_file} files, {n_link} symlinks -> {DIST}")


if __name__ == "__main__":
    if len(sys.argv) > 1:
        zips = [sys.argv[1]]
    else:
        zips = sorted(glob.glob(os.path.join(APP, "electron-cache", "*.zip")))
        if not zips:
            sys.exit("no zip found in electron-cache/")
    install(zips[0])
