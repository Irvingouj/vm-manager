#!/usr/bin/env python3
"""Renders the frames of every scene as 1920x1080 PNGs, with a headless Firefox
(media/capture/browser.py), from what capture.py recorded:

- a terminal scene: the recorded commands "typed" over a few frames, then their
  real output revealed in steps (the output was masked at capture);
- a browser scene: the real screenshot in a window frame that names its real
  address, then the same with the region the narration points at outlined;
- a card: title and end cards, in the same look.

    python3 media/render.py
Writes $T74/frames/<scene>/NNN.png and $T74/frames/<scene>.json: [[png, kind], ...]
where kind is "type" (a short typing frame), "out", "shot" or "card".
"""
import html, json, os, shutil, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "capture"))
import browser

T = os.environ.get("T74", "/tmp/t74")
R = os.path.expanduser("~/snap/firefox/common/t74/render")   # the snap Firefox reads only under ~/snap
BG, ACCENT, FG, MUTED = "#0e1117", "#f5a524", "#e8eaed", "#9aa0a6"
REPO = "github.com/Irvingouj/vm-manager"

CSS = f"""
html,body{{margin:0;width:1280px;height:720px;overflow:hidden;background:{BG};color:{FG};font-family:Ubuntu,sans-serif}}
.win{{position:absolute;left:107px;top:16px;width:1066px;height:590px;border-radius:10px;overflow:hidden;
      box-shadow:0 10px 40px rgba(0,0,0,.6);border:1px solid #2a2f3a}}
.bar{{height:28px;background:#1c2029;display:flex;align-items:center;padding:0 12px;gap:7px;font-size:12px;color:{MUTED}}}
.dot{{width:11px;height:11px;border-radius:50%;background:#3a3f4b}}
.title{{margin-left:14px}}
.term{{background:#12151c;height:562px;padding:10px 16px;box-sizing:border-box;font-family:'Ubuntu Mono',monospace;
       font-size:15.5px;line-height:1.3;white-space:pre-wrap;word-break:break-all;overflow:hidden}}
.p{{color:{ACCENT}}} .c{{color:#ffffff;font-weight:bold}} .o{{color:#c9ccd1}}
.cursor{{display:inline-block;width:8px;height:15px;background:{ACCENT};vertical-align:text-bottom}}
.url{{flex:1;margin-left:12px;background:#0e1117;border-radius:6px;padding:3px 10px;color:{FG};font-size:12.5px;font-family:Ubuntu,sans-serif}}
.page{{position:relative;width:1066px;height:600px;overflow:hidden;background:#fff}}
.page img{{position:absolute;left:0;top:0;width:1066px;height:600px}}
.mark{{position:absolute;border:3px solid {ACCENT};border-radius:8px;box-shadow:0 0 0 4000px rgba(0,0,0,.28)}}
.brand{{position:absolute;left:24px;bottom:14px;font-size:12px;color:{MUTED};letter-spacing:.08em}}
.brand b{{color:{ACCENT}}}
.card{{position:absolute;left:120px;top:0;height:600px;width:1040px;display:flex;flex-direction:column;justify-content:center}}
.card h1{{font-size:54px;font-weight:500;margin:0 0 14px}} .card h2{{font-size:24px;font-weight:300;color:{MUTED};margin:0 0 26px;line-height:1.35}}
.card .rule{{width:90px;height:4px;background:{ACCENT};margin-bottom:28px}}
.card p{{font-size:19px;line-height:1.5;margin:0 0 10px;color:{FG}}} .card .small{{font-size:16px;color:{MUTED}}}
.card .acc{{color:{ACCENT}}}
"""


def page(body):
    return f"<!doctype html><html><head><meta charset=utf-8><style>{CSS}</style></head><body>{body}" \
           f"<div class=brand><b>VIBE MANAGER</b> 0.1</div></body></html>"


def wrap_lines(text, width=118):
    out = []
    for line in text.split("\n"):
        while len(line) > width:
            out.append(line[:width])
            line = line[width:]
        out.append(line)
    return out


def term_html(prompt, done, current=None, typed=None, out_upto=None, max_lines=28):
    """done: [(prompt, cmd, out)] shown whole; current: (prompt, cmd, out) being typed or revealed."""
    blocks = []  # (css class, text) lines
    def add(p, cmd, out):
        cl = cmd.split("\n")
        blocks.append([("p", p + " "), ("c", cl[0])])
        for extra in cl[1:]:
            blocks.append([("c", extra)])
        if out:
            for l in wrap_lines(out):
                blocks.append([("o", l)])
    for p, c, o in done:
        add(p, c, o)
    cur_line = None
    if current:
        p, c, o = current
        if typed is not None:
            add(p, c[:typed], None)
            cur_line = len(blocks) - 1
        else:
            lines = o.split("\n") if o else []
            add(p, c, "\n".join(lines[:out_upto]) if out_upto else "")
    if current is None or typed is None:
        blocks.append([("p", prompt + " ")])
        cur_line = len(blocks) - 1
    blocks = blocks[-max_lines:]
    cur_line = cur_line - (len(blocks) - max_lines) if cur_line is not None and cur_line >= 0 else None
    rows = []
    for i, b in enumerate(blocks):
        s = "".join(f"<span class={k}>{html.escape(t)}</span>" for k, t in b)
        if i == len(blocks) - 1 and (cur_line is None or True):
            s += "<span class=cursor></span>"
        rows.append(s)
    title = prompt.split(":")[0]
    return page(f"<div class=win><div class=bar><span class=dot></span><span class=dot></span><span class=dot></span>"
                f"<span class=title>{html.escape(title)} — terminal</span></div><div class=term>{'<br>'.join(rows)}</div></div>")


def shot_html(img, url, marks=(), crop=None):
    # The screenshot is 1920x1080 device px; it is shown at 1066x600 CSS px.
    sc = 1066 / 1920
    style = ""
    if crop:  # [x, y, w] in screenshot px: that region fills the window (the page's content is small)
        cx, cy, cw = crop
        z = 1920 / cw
        style = f"style='width:{1066*z}px;height:{600*z}px;left:{-cx*sc*z}px;top:{-cy*sc*z}px'"
        sc = sc * z
        marks = [[(m[0] - cx), (m[1] - cy), m[2], m[3]] for m in marks]
    boxes = "".join(f"<div class=mark style='left:{m[0]*sc-6}px;top:{m[1]*sc-6}px;width:{m[2]*sc+6}px;height:{m[3]*sc+6}px'></div>"
                    for m in marks)
    return page(f"<div class=win style='height:628px'><div class=bar><span class=dot></span><span class=dot></span><span class=dot></span>"
                f"<span class=url>{html.escape(url)}</span></div><div class=page><img src='{img}' {style}>{boxes}</div></div>")


def card_html(h1, h2="", lines=(), small=""):
    ps = "".join(f"<p>{l}</p>" for l in lines)
    sm = f"<p class=small>{small}</p>" if small else ""
    return page(f"<div class=card><h1>{h1}</h1><h2>{h2}</h2><div class=rule></div>{ps}{sm}</div>")


CARDS = {
    "d01": card_html("Vibe Manager <span class=acc>0.1</span>", "From an install to an Application shared with a colleague",
                     ["A demo for a fictional company, Acme, at <span class=acc>vibe.acme.test</span>",
                      "Ada: Platform administrator and Builder &nbsp;·&nbsp; Grace: a colleague"],
                     "Real commands, real output, a real browser. Secrets are masked with •"),
    "d16": card_html("Install. Sign in. Build.<br>Run. Deploy. <span class=acc>Share.</span>", "Vibe Manager 0.1",
                     ["Not yet for untrusted Applications or real company data.",
                      "What is missing is in the release notes: docs/releases/v0.1.0.md"],
                     f"Getting started: docs/getting-started.md &nbsp;·&nbsp; {REPO}"),
    "l01": card_html("Vibe <span class=acc>Manager</span>", "Where your company's AI-built internal tools run",
                     [], "Open source &nbsp;·&nbsp; self-hosted"),
    "l06": card_html("Shared databases,<br><span class=acc>governed by IT</span>", "",
                     ["An Application reads and writes a shared database only as its Grants allow,",
                      "as the person using it.",
                      "Every operation is in the Operation log; every row it changes in its own",
                      "database is in the Change history."]),
    "l07": card_html("Vibe Manager <span class=acc>0.1</span>", "Open source &nbsp;·&nbsp; self-hosted",
                     ["A first release: for Applications you wrote yourself, and data you can lose."],
                     f"{REPO}"),
}

# Shots whose page holds a line of text at the top left: shown zoomed on that corner.
SMALL = ("dev-page", "hello-ada", "grace-hello")


def frames_for(scene, steps):
    out = []  # (html, kind)
    done = []
    prompt = next((s["prompt"] for s in steps if s["kind"] == "cmd"), "$")
    for s in steps:
        if s["kind"] == "cmd":
            p, c, o = s["prompt"], s["cmd"], s["out"]
            n = len(c)
            for k in sorted({max(1, n // 3), max(1, 2 * n // 3)}):
                out.append((term_html(p, done, (p, c, o), typed=k), "type"))
            lines = o.split("\n") if o else []
            chunks = [len(lines)] if len(lines) <= 6 else [len(lines) // 2, len(lines)]
            if not lines:
                chunks = [0]
            for u in chunks:
                out.append((term_html(p, done, (p, c, o), out_upto=u), "out"))
            done.append((p, c, o))
            prompt = p
        else:
            name = os.path.basename(s["png"])
            shutil.copy(s["png"], os.path.join(R, name))
            crop = [0, 0, 960] if any(k in name for k in SMALL) else None
            out.append((shot_html(name, s["url"], (), crop), "shot"))
            if s["marks"]:
                out.append((shot_html(name, s["url"], s["marks"], crop), "shot"))
    return out


def main():
    os.makedirs(R, exist_ok=True)
    f = browser.start("ada", 2841)
    scenes = sorted(set(list(CARDS) + [n[:-5] for n in os.listdir(f"{T}/rec")]))
    want = set(sys.argv[1:])
    for sc in scenes:
        if want and sc not in want:
            continue
        if sc in CARDS:
            items = [(CARDS[sc], "card")]
        else:
            items = frames_for(sc, json.load(open(f"{T}/rec/{sc}.json")))
        d = f"{T}/frames/{sc}"
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
        manifest = []
        for i, (h, kind) in enumerate(items):
            hp = os.path.join(R, f"{sc}-{i:03d}.html")
            open(hp, "w").write(h)
            f.go("file://" + hp)
            png = f"{d}/{i:03d}.png"
            f.shot(png, full=False)
            manifest.append([png, kind])
        json.dump(manifest, open(f"{d}.json", "w"))
        print("rendered", sc, len(items), "frames")


if __name__ == "__main__":
    main()
