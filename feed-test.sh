#!/usr/bin/env bash
# Prove the loopback path works before involving Howdy.
#
# Howdy reads V4L2 through OpenCV, which cannot decode ip3y (IPU3-packed 10-bit
# greyscale). So irface.py unpacks frames and republishes them as 8-bit GREY on
# a v4l2loopback node, which is what Howdy actually opens.
#
# Verifies by pulling a frame back OUT of the loopback and checking it contains
# a face -- not merely that the device exists.
set -uo pipefail
[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

NODE=${NODE:-20}
DEV=/dev/video$NODE
CUR=${CUR:-1}
GAIN=${GAIN:-128}

echo "=== loading v4l2loopback ==="
modprobe -r v4l2loopback 2>/dev/null
modprobe v4l2loopback devices=1 video_nr=$NODE card_label="Howdy IR" exclusive_caps=1 \
  || { echo "modprobe failed" >&2; exit 1; }
sleep 1
ls -l "$DEV" || exit 1

echo "=== starting feeder (20s) ==="
/home/ukv/cement-ir/irface.py --gain "$GAIN" --current "$CUR" \
    feed --device "$DEV" --duration 20 --fps 10 &
FEEDER=$!
trap 'kill $FEEDER 2>/dev/null; /home/ukv/cement-ir/irface.py --current 0 shot --out /dev/null >/dev/null 2>&1' EXIT
sleep 5

echo "=== what does the loopback advertise? ==="
v4l2-ctl -d "$DEV" --all 2>/dev/null | grep -iE "Width/Height|Pixel Format|Card type" | head -4

echo "=== pulling a frame back out of the loopback ==="
rm -f /tmp/loopgrab.png
ffmpeg -loglevel error -f v4l2 -i "$DEV" -frames:v 1 -y /tmp/loopgrab.png 2>&1 | head -3
ls -l /tmp/loopgrab.png 2>&1

echo "=== does dlib find a face in what Howdy would see? ==="
python3 - <<'PY'
import numpy as np, dlib, pathlib, sys
try:
    import cv2
except ImportError:
    sys.exit("  (opencv missing)")
img = cv2.imread("/tmp/loopgrab.png", cv2.IMREAD_GRAYSCALE)
if img is None:
    sys.exit("  !! could not read the grabbed frame")
print(f"  frame {img.shape} mean {img.mean():.1f}")
rgb = np.stack([img]*3, -1).copy()
det = dlib.get_frontal_face_detector()
faces, scores, _ = det.run(rgb, 1, -0.5)
if len(faces):
    print(f"  OK: {len(faces)} face(s), best score {max(scores):.3f}")
else:
    print("  !! no face detected in the loopback frame")
PY

wait $FEEDER 2>/dev/null
chown ukv:ukv /tmp/loopgrab.png 2>/dev/null
echo "=== done ==="
