#!/usr/bin/env python3
"""Re-score the tuning frames with YuNet, the detector howdy-next actually uses.

The earlier ranking used dlib's HOG frontal detector, because howdy 2.6.1 used
dlib. howdy-next is a C++/OpenCV rewrite using YuNet for detection and SFace for
recognition -- a different model with different exposure sensitivity. Carrying
the old operating point over unverified would be assuming the criterion instead
of measuring it.
"""
import pathlib, sys
import numpy as np, cv2

MODEL = "/usr/share/howdy/models/face_detection_yunet_2026may.onnx"

def read_pgm(p):
    d = p.read_bytes(); parts, pos = [], 0
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
    return np.frombuffer(d, np.uint8, count=w*h, offset=pos).reshape(h, w)

det = cv2.FaceDetectorYN.create(MODEL, "", (640, 480), 0.5, 0.3, 5000)
print(f"  {'setting':22s} {'faces':>5s} {'score':>7s}")
best = []
for p in sorted(pathlib.Path(sys.argv[1]).glob("*.pgm")):
    img = read_pgm(p)
    bgr = cv2.cvtColor(img, cv2.COLOR_GRAY2BGR)
    det.setInputSize((bgr.shape[1], bgr.shape[0]))
    _, faces = det.detect(bgr)
    if faces is not None and len(faces):
        sc = float(max(f[-1] for f in faces))
        print(f"  {p.stem:22s} {len(faces):5d} {sc:7.3f}   <-- usable")
        best.append((sc, p.stem))
    else:
        print(f"  {p.stem:22s} {0:5d} {'--':>7s}")
if best:
    best.sort(reverse=True)
    print(f"\n  BEST for YuNet: {best[0][1]}  (score {best[0][0]:.3f})")
