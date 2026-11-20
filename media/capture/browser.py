"""Starts a headless Firefox (the snap) with a profile of its own under
~/snap/firefox/common/t74/<name>, Marionette on a given port, the rig's test
domain resolved to 127.0.0.1, 1.5 device pixels per CSS pixel (a 1280x720
viewport gives 1920x1080 screenshots), and the rig's certificate authority
trusted. Its pid goes to $T74/run/firefox-<name>.pid (rig/down.sh ends it)."""
import os, socket, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ff import Firefox

T = os.environ.get("T74", "/tmp/t74")
BASE = os.path.expanduser("~/snap/firefox/common/t74")
NAMES = ["idp", "api", "auth", "dashboard", "hello"]

PREFS = {
    "network.dns.localDomains": ",".join(f"{n}.vibe.acme.test" for n in NAMES),
    "dom.security.https_only_mode": False,
    "browser.shell.checkDefaultBrowser": False,
    "datareporting.policy.dataSubmissionEnabled": False,
    "toolkit.telemetry.reportingpolicy.firstRun": False,
    "browser.aboutwelcome.enabled": False,
    "app.update.auto": False,
    "signon.rememberSignons": False,
    "layout.css.devPixelsPerPx": "1.5",
    "ui.prefersReducedMotion": 1,
    "font.name.sans-serif.x-western": "Ubuntu",
}


def _pref(k, v):
    if isinstance(v, bool):
        return f'user_pref("{k}", {"true" if v else "false"});'
    if isinstance(v, int):
        return f'user_pref("{k}", {v});'
    return f'user_pref("{k}", "{v}");'


def start(name, port):
    prof = os.path.join(BASE, name)
    os.makedirs(prof, exist_ok=True)
    prefs = dict(PREFS, **{"marionette.port": port})
    open(os.path.join(prof, "user.js"), "w").write("\n".join(_pref(k, v) for k, v in prefs.items()) + "\n")
    os.makedirs(os.path.join(T, "run"), exist_ok=True)
    pidf = os.path.join(T, "run", f"firefox-{name}.pid")
    if not _open(port):
        env = dict(os.environ, MOZ_REMOTE_ALLOW_SYSTEM_ACCESS="1", MOZ_HEADLESS="1")
        log = open(os.path.join(BASE, f"{name}.log"), "w")
        p = subprocess.Popen(["firefox", "--headless", "--marionette", "--no-remote", "--profile", prof],
                             env=env, stdout=log, stderr=log, start_new_session=True)
        open(pidf, "w").write(str(p.pid))
        for _ in range(120):
            if _open(port):
                break
            time.sleep(0.5)
        time.sleep(1.0)
    f = Firefox(port)
    pem = open(os.path.join(T, "pki", "ca.pem")).read()
    b64 = "".join(l for l in pem.splitlines() if "CERTIFICATE" not in l)
    f.chrome(f'''const db = Cc["@mozilla.org/security/x509certdb;1"].getService(Ci.nsIX509CertDB);
      db.addCertFromBase64("{b64}", "C,,"); return 1;''')
    f.size(1280, 720)
    inner = f.js("return [window.innerWidth, window.innerHeight]")
    f.size(1280 + 1280 - inner[0], 720 + 720 - inner[1])
    return f


def _open(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
        return True
    except OSError:
        return False
