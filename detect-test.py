#!/usr/bin/env python3
"""Score each tuning frame by what actually matters: can dlib find a face?

Mean brightness and clipping percentage are proxies. The operating point that
counts is the one where the face detector fires and the landmark predictor gets
a clean fit -- so test that directly instead of inferring it from exposure.
"""
import sys, pathlib
import numpy as np, dlib

DATA = pathlib.Path("/tmp/howdysrc/usr/lib/security/howdy/dlib-data")

def read_pgm(p):
    d = p.read_bytes()
    parts, pos = [], 0
    while len(parts) < 4:
        while d[pos:pos+1].isspace(): pos += 1
        if d[pos:pos+1] == b"#":
            while d[pos:pos+1] != b"\n": pos += 1
            continue
        st = pos
        while not d[pos:pos+1].isspace(): pos += 1
        parts.append(d[st:pos])
    pos += 1
    w, h = int(parts[1]), int(parts[2])
    return np.frombuffer(d, dtype=np.uint8, count=w*h, offset=pos).reshape(h, w)

det = dlib.get_frontal_face_detector()
sp = dlib.shape_predictor(str(DATA / "shape_predictor_5_face_landmarks.dat"))
enc = dlib.face_recognition_model_v1(str(DATA / "dlib_face_recognition_resnet_model_v1.dat"))

rows = []
for p in sorted(pathlib.Path(sys.argv[1]).glob("*.pgm")):
    img = read_pgm(p)
    rgb = np.stack([img]*3, axis=-1).copy()
    faces, scores, _ = det.run(rgb, 1, -0.5)
    if len(faces):
        best = int(np.argmax(scores))
        shape = sp(rgb, faces[best])
        vec = np.array(enc.compute_face_descriptor(rgb, shape))
        rows.append((p.stem, len(faces), float(scores[best]), float(np.linalg.norm(vec))))
    else:
        rows.append((p.stem, 0, float("nan"), float("nan")))

print(f"  {'setting':22s} {'faces':>5s} {'score':>7s}   {'encoding':>8s}")
for name, n, sc, norm in rows:
    mark = "  <-- usable" if n and sc > 0.3 else ("  (weak)" if n else "")
    print(f"  {name:22s} {n:5d} {sc:7.3f}   {norm:8.3f}{mark}")
