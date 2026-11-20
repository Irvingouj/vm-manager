#!/usr/bin/env python3
"""Narration: one WAV per scene of demo-script.md and launch-script.md, by
DeepInfra's text-to-speech (hexgrad/Kokoro-82M, voice af_heart).

Only the narration text is sent. The key is DEEPINFRA_API_KEY in Infisical; it
is read inside the shell's command substitution of the curl call and never
enters this program, a file or a log.

    python3 media/tts.py [scene-id ...]     # all scenes without a WAV yet, or the ones named
Writes $T74/audio/<id>.wav (default /tmp/t74).
"""
import base64, json, os, re, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
T = os.environ.get("T74", "/tmp/t74")
MODEL, VOICE, SPEED = "hexgrad/Kokoro-82M", "af_heart", 0.95
PROJECT = os.environ.get("INFISICAL_PROJECT_ID", "0b692fb3-11c6-483a-9083-2fb8bf32796c")


def scenes(path):
    """[(id, kind, shows, narration)] from a script file."""
    out, cur = [], None
    for line in open(path):
        m = re.match(r"^## (\S+) (\S+)", line)
        if m:
            cur = [m.group(1), m.group(2), "", ""]
            out.append(cur)
        elif cur and line.startswith("Shows:"):
            cur[2] = line[6:].strip()
        elif cur and line.startswith(">"):
            cur[3] = (cur[3] + " " + line[1:].strip()).strip()
    return [tuple(s) for s in out]


def all_scenes():
    return scenes(os.path.join(HERE, "demo-script.md")) + scenes(os.path.join(HERE, "launch-script.md"))


def speak(text, wav):
    body = json.dumps({"text": text, "preset_voice": [VOICE], "output_format": "wav", "speed": SPEED})
    with tempfile.TemporaryDirectory(dir=T) as tmp:
        req, resp = os.path.join(tmp, "req.json"), os.path.join(tmp, "resp")
        open(req, "w").write(body)
        cmd = ('curl -sS --fail-with-body --max-time 120 -X POST '
               f'"https://api.deepinfra.com/v1/inference/{MODEL}" '
               '-H "Content-Type: application/json" '
               f'-H "Authorization: Bearer $(infisical secrets get DEEPINFRA_API_KEY --projectId={PROJECT} --env=dev --plain --silent)" '
               '--data-binary @"$1" -o "$2"')
        r = subprocess.run(["bash", "-c", cmd, "tts", req, resp], capture_output=True, text=True, timeout=180)
        data = open(resp, "rb").read() if os.path.exists(resp) else b""
        if r.returncode != 0:
            raise SystemExit(f"text-to-speech failed (exit {r.returncode}): {data[:300]!r}")
        if data[:4] == b"RIFF":
            audio = data
        else:
            j = json.loads(data)
            a = j.get("audio") or ""
            audio = base64.b64decode(a.split(",", 1)[1] if a.startswith("data:") else a)
        open(wav, "wb").write(audio)


def main():
    os.makedirs(os.path.join(T, "audio"), exist_ok=True)
    want = set(sys.argv[1:])
    for sid, _kind, _shows, text in all_scenes():
        wav = os.path.join(T, "audio", f"{sid}.wav")
        if (want and sid not in want) or (not want and os.path.exists(wav)):
            continue
        speak(text, wav)
        print("spoke", sid)


if __name__ == "__main__":
    main()
