#!/usr/bin/env python3
"""Assembles demo.mp4 and launch.mp4 (1920x1080, 30 fps, H.264 + AAC) with ffmpeg from
the rendered frames ($T74/frames) and the narration ($T74/audio):

- each scene lasts as long as its narration plus a short pause; its frames share
  that time (typing frames short), stills and cards with a slow zoom;
- scenes joined by crossfades (video and audio);
- captions from the narration: burnt in, and an .srt beside each video;
- loudness normalised (loudnorm, -16 LUFS).

    python3 media/assemble.py [demo|launch ...]
Writes $T74/out/{demo,launch}.{mp4,srt} and $T74/out/poster.png.
"""
import json, os, re, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import tts

T = os.environ.get("T74", "/tmp/t74")
FF = os.environ.get("FFMPEG", os.path.expanduser("~/.local/bin/ffmpeg"))
FP = os.environ.get("FFPROBE", os.path.expanduser("~/.local/bin/ffprobe"))
LEAD, TAIL, XF, TYPE = 0.35, 0.9, 0.5, 0.22

# The launch video's scenes cut from the demo's frames: (demo scene, first, last) by frame index.
LAUNCH_CUTS = {
    "l02": [("d03", -4, None), ("d06", 0, None)],
    "l03": [("d09", 0, 4), ("d10", 0, None)],
    "l04": [("d11", 2, None), ("d12", 0, None)],
    "l05": [("d13", -1, None), ("d14", 0, None), ("d15", 0, None)],
}


def dur(path):
    return float(subprocess.check_output([FP, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path]).strip())


def frames(scene):
    if scene in LAUNCH_CUTS:
        out = []
        for src, a, b in LAUNCH_CUTS[scene]:
            out += json.load(open(f"{T}/frames/{src}.json"))[a:b]
        return out
    return json.load(open(f"{T}/frames/{scene}.json"))


def timing(items, length):
    weight = {"out": 1.0, "shot": 1.7, "card": 1.0}
    fixed = sum(TYPE for _, k in items if k == "type")
    total_w = sum(weight.get(k, 0) for _, k in items if k != "type") or 1
    rest = max(length - fixed, 0.5)
    return [TYPE if k == "type" else rest * weight[k] / total_w for _, k in items]


def scene_clip(sid, out):
    a = f"{T}/audio/{sid}.wav"
    length = LEAD + dur(a) + TAIL
    items = frames(sid)
    ds = timing(items, length)
    ds[-1] += length - sum(ds)  # rounding: the scene lasts exactly its length
    args, filt = [FF, "-v", "error", "-y"], []
    for i, ((png, kind), d) in enumerate(zip(items, ds)):
        args += ["-loop", "1", "-framerate", "30", "-t", f"{d:.3f}", "-i", png]
        n = max(int(round(d * 30)), 1)
        if kind in ("shot", "card") and d >= 1.5:
            filt.append(f"[{i}]scale=2880:1620,zoompan=z='1+0.03*on/{n}':x='iw/2-iw/zoom/2':y='ih/2-ih/zoom/2':d=1:s=1920x1080:fps=30,setsar=1,format=yuv420p[v{i}]")
        else:
            filt.append(f"[{i}]scale=1920:1080,fps=30,setsar=1,format=yuv420p[v{i}]")
    k = len(items)
    args += ["-i", a]
    filt.append("".join(f"[v{i}]" for i in range(k)) + f"concat=n={k}:v=1:a=0,trim=duration={length:.3f}[v]")
    filt.append(f"[{k}]aresample=48000,aformat=channel_layouts=stereo,adelay={int(LEAD*1000)}:all=1,apad=whole_dur={length:.3f}[a]")
    args += ["-filter_complex", ";".join(filt), "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-preset", "veryfast",
             "-crf", "18", "-r", "30", "-c:a", "pcm_s16le", "-t", f"{length:.3f}", out]
    subprocess.run(args, check=True)
    return length


def chunks(text, limit=72):
    """The narration as caption lines: sentences, split at a comma or colon when long."""
    out = []
    for s in re.split(r"(?<=[.?!])\s+", text.strip()):
        while len(s) > limit:
            cut = max(s.rfind(", ", 0, limit), s.rfind(": ", 0, limit), s.rfind(" ", 0, limit) if s.rfind(", ", 0, limit) < 20 else -1)
            cut = cut if cut > 15 else s.rfind(" ", 0, limit)
            out.append(s[:cut + 1].strip())
            s = s[cut + 1:].strip()
        out.append(s)
    return [c for c in out if c]


def ts(t):
    ms = int(round(t * 1000))
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d},{ms % 1000:03d}"


def build(name, ids):
    os.makedirs(f"{T}/clips", exist_ok=True)
    os.makedirs(f"{T}/out", exist_ok=True)
    text = {s[0]: s[3] for s in tts.all_scenes()}
    lengths, srt, start, n = [], [], 0.0, 1
    for sid in ids:
        L = scene_clip(sid, f"{T}/clips/{sid}.mkv")
        a = dur(f"{T}/audio/{sid}.wav")
        parts = chunks(text[sid])
        total = sum(len(p) for p in parts)
        t = start + LEAD
        for p in parts:
            d = a * len(p) / total
            srt.append(f"{n}\n{ts(t)} --> {ts(t + d - 0.05)}\n{p}\n")
            n += 1
            t += d
        lengths.append(L)
        start += L - XF
        print("clip", sid, f"{L:.2f}s")
    srt_path = f"{T}/out/{name}.srt"
    open(srt_path, "w").write("\n".join(srt))
    args = [FF, "-v", "error", "-y"]
    for sid in ids:
        args += ["-i", f"{T}/clips/{sid}.mkv"]
    filt, v, a, acc = [], "[0:v]", "[0:a]", lengths[0]
    for i in range(1, len(ids)):
        off = acc - XF
        filt.append(f"{v}[{i}:v]xfade=transition=fade:duration={XF}:offset={off:.3f}[x{i}]")
        filt.append(f"{a}[{i}:a]acrossfade=d={XF}[y{i}]")
        v, a = f"[x{i}]", f"[y{i}]"
        acc += lengths[i] - XF
    style = ("FontName=Ubuntu,FontSize=12,PrimaryColour=&H00FFFFFF,OutlineColour=&H99000000,BackColour=&H99000000,"
             "BorderStyle=3,Outline=4,Shadow=0,MarginV=6,Alignment=2")
    filt.append(f"{v}subtitles={srt_path}:force_style='{style}',format=yuv420p[vo]")
    filt.append(f"{a}loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000[ao]")
    out = f"{T}/out/{name}.mp4"
    args += ["-filter_complex", ";".join(filt), "-map", "[vo]", "-map", "[ao]", "-c:v", "libx264", "-preset", "medium",
             "-crf", "20", "-r", "30", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "160k", "-ar", "48000",
             "-movflags", "+faststart", out]
    subprocess.run(args, check=True)
    print("made", out, f"{dur(out):.1f}s")


def main():
    scenes = tts.all_scenes()
    want = sys.argv[1:] or ["demo", "launch"]
    if "demo" in want:
        build("demo", [s[0] for s in scenes if s[0].startswith("d")])
        subprocess.run(["cp", f"{T}/frames/d01/000.png", f"{T}/out/poster.png"], check=True)
    if "launch" in want:
        build("launch", [s[0] for s in scenes if s[0].startswith("l")])


if __name__ == "__main__":
    main()
