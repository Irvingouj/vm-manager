#!/usr/bin/env python3
"""Captures the demo for real: runs each command of getting-started.md against the
rig's installation of v0.1.0 and records its real output (secrets masked before
anything is written), and drives two headless Firefoxes (Ada's and Grace's)
over Marionette, taking screenshots.

    python3 media/capture/capture.py            # every scene, in order
    python3 media/capture/capture.py d09 d10    # the scenes named (later ones need the earlier)

Writes $T74/rec/<scene>.json: a list of steps, each
  {"kind": "cmd", "prompt": ..., "cmd": ..., "out": ...}  or
  {"kind": "shot", "png": ..., "url": ..., "marks": [[x, y, w, h], ...]}
"""
import json, os, re, socket, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import browser

HERE = os.path.dirname(os.path.abspath(__file__))
T = os.environ.get("T74", "/tmp/t74")
REL = "https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0"
D, PORT, IDP = "vibe.acme.test", 18443, 18444
WHERE = f"--domain {D} --port {PORT} --resolve 127.0.0.1"
SRV = f"{T}/opt/vibe"            # the "server"'s install folder
HOME = f"{T}/home"               # the Builder's computer: the tool's HOME
PROMPT_SRV = "admin@acme-server:~/vibe$"
NODE = os.path.expanduser("~/.nvm/versions/node/v22.23.0/bin")


def secret(name):
    p = f"{T}/kc/{name}"
    return open(p).read().strip() if os.path.exists(p) else None


def mask(text):
    """Every secret of the rig, and anything that looks like a credential, replaced by •."""
    for n in ["client-secret", "admin-password", "ada-password", "grace-password", "setup-credential"]:
        s = secret(n)
        if s:
            text = text.replace(s, "•" * 12)
    text = re.sub(r"\b(vm[a-z]{1,3}_)[A-Za-z0-9_\-]{12,}", lambda m: m.group(1) + "•" * 24, text)
    text = re.sub(r"(VIBE_SECRET_KEY=|POSTGRES_PASSWORD=)[A-Za-z0-9+/=]{16,}", lambda m: m.group(1) + "•" * 12, text)
    text = text.replace(socket.gethostname(), "ada-laptop")
    text = re.sub(r"challenge=[^&\s]+", "challenge=" + "•" * 12, text)
    home = os.path.expanduser("~")
    for real, shown in [(f"{SRV}", "~/vibe"), (HOME, "~"), (T, "~"), (home, "~")]:
        text = text.replace(real, shown)
    text = re.sub(r"\b%s\b" % re.escape(os.environ.get("USER", "nobody")), "user", text)
    return text


def clean(out):
    """Terminal output as a terminal would leave it: a carriage return overwrites the line."""
    lines = []
    for line in out.replace("\r\n", "\n").split("\n"):
        parts = [p for p in line.split("\r") if p.strip()]
        lines.append(parts[-1] if parts else "")
    out = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", "\n".join(lines))
    return out.strip("\n")


def env_tool():
    return dict(os.environ, HOME=HOME, VIBE_CA_FILE=f"{HOME}/acme-ca.pem",
                PATH=f"{HOME}/.local/bin:{NODE}:" + os.environ["PATH"], NO_COLOR="1")


def env_srv():
    return dict(os.environ, COMPOSE_PROJECT_NAME="t74-vibe", COMPOSE_FILE="compose.yaml:compose.t74.yaml", NO_COLOR="1")


class Rec:
    def __init__(self, scene):
        self.scene, self.steps = scene, []

    def run(self, cmd, cwd, env, prompt, shown=None, stdin=None, timeout=600, ok=(0,), laptop=False):
        r = subprocess.run(as_laptop(cmd) if laptop else ["bash", "-c", cmd], cwd=cwd, env=env, capture_output=True, text=True,
                           timeout=timeout, input=stdin)
        out = clean(r.stdout + ("\n" if r.stdout and r.stderr else "") + r.stderr)
        if r.returncode not in ok:
            raise SystemExit(f"{self.scene}: `{mask(shown or cmd)}` exited {r.returncode}:\n{mask(out)[-1500:]}")
        self.steps.append({"kind": "cmd", "prompt": prompt, "cmd": mask(shown or cmd), "out": mask(out)})
        return r

    def note(self, prompt, cmd, out):
        self.steps.append({"kind": "cmd", "prompt": prompt, "cmd": mask(cmd), "out": mask(clean(out))})

    def shot(self, f, name, marks=()):
        os.makedirs(f"{T}/shots", exist_ok=True)
        png = f"{T}/shots/{self.scene}-{len(self.steps):02d}-{name}.png"
        time.sleep(0.4)
        f.shot(png, full=False)
        rects = []
        for css in marks:
            for e in f.find(css)[:1]:
                rects.append([round(v * 1.5) for v in f.rect(e)])
        self.steps.append({"kind": "shot", "png": png, "url": f.url().split("?")[0], "marks": rects})

    def save(self):
        os.makedirs(f"{T}/rec", exist_ok=True)
        json.dump(self.steps, open(f"{T}/rec/{self.scene}.json", "w"), indent=1)
        print("recorded", self.scene, len(self.steps), "steps")


def as_laptop(cmd):
    """The Builder's computer: the tool names the machine it runs on (its host name) to
    the sign-in page, so it runs in a namespace of its own whose host name is ada-laptop."""
    return ["unshare", "--user", "--map-current-user", "--uts", "--keep-caps", "bash", "-c", "hostname ada-laptop && " + cmd]


def ptool(cwd):
    rel = os.path.relpath(cwd, HOME)
    return "ada@ada-laptop:~$" if rel == "." else f"ada@ada-laptop:~/{rel}$"


def tool(rec, cmd, cwd=HOME, **kw):
    return rec.run(cmd, cwd, env_tool(), ptool(cwd), laptop=True, **kw)


def srv(rec, cmd, **kw):
    return rec.run(cmd, SRV, env_srv(), PROMPT_SRV, **kw)


CURL_API = f"curl -s --cacert ca/acme-ca.pem --resolve api.{D}:{PORT}:127.0.0.1 https://api.{D}:{PORT}/v1/installation"

_ff = {}


def ff(who):
    if who not in _ff:
        _ff[who] = browser.start(who, {"ada": 2841, "grace": 2842}[who])
    return _ff[who]


def provider_sign_in(rec, f, who):
    if f"idp.{D}" not in f.url():
        return
    rec.shot(f, f"{who}-provider", marks=["#username"])
    f.type(f.find("#username")[0], f"{who}@acme.test")
    f.type(f.find("#password")[0], secret(f"{who}-password"))
    rec.shot(f, f"{who}-typed", marks=["#kc-login"])
    f.click(f.find("#kc-login")[0])
    time.sleep(2.5)


# ---------------------------------------------------------------- scenes

def d02(rec):
    os.makedirs(f"{SRV}/certs", exist_ok=True)
    os.makedirs(f"{SRV}/ca", exist_ok=True)
    srv(rec, "\n".join(f"curl -fsSLO {REL}/{n}" for n in ["compose.yaml", "env.example", "SHA256SUMS"]))
    srv(rec, "sha256sum --check --ignore-missing SHA256SUMS")


def d03(rec):
    # The rig's part, not shown: its own overlay (no DNS), the wildcard certificate and the CA.
    subprocess.run(["cp", f"{HERE}/../rig/compose.t74.yaml", SRV], check=True)
    subprocess.run(["cp", f"{T}/pki/wild.pem", f"{SRV}/certs/fullchain.pem"], check=True)
    subprocess.run(["cp", f"{T}/pki/wild.key", f"{SRV}/certs/privkey.pem"], check=True)
    os.chmod(f"{SRV}/certs/fullchain.pem", 0o644)
    os.chmod(f"{SRV}/certs/privkey.pem", 0o644)  # throwaway key; no sudo here to give it to 65532
    subprocess.run(["cp", f"{T}/pki/ca.pem", f"{SRV}/ca/acme-ca.pem"], check=True)
    os.chmod(f"{SRV}/ca/acme-ca.pem", 0o644)
    for c in ["cp env.example .env", "chmod 600 .env",
              f'sed -i "s|^VIBE_DOMAIN=.*|VIBE_DOMAIN={D}|" .env',
              'sed -i "s|^VIBE_SECRET_KEY=.*|VIBE_SECRET_KEY=$(openssl rand -base64 32)|" .env',
              'sed -i "s|^POSTGRES_PASSWORD=.*|POSTGRES_PASSWORD=$(openssl rand -hex 24)|" .env',
              f'echo "VIBE_HTTPS_PORT={PORT}" >> .env',
              'echo "VIBE_CA_FILE=/etc/vibe/ca/acme-ca.pem" >> .env']:
        srv(rec, c)
    srv(rec, "docker compose up -d", timeout=300)
    for _ in range(90):
        r = subprocess.run(["bash", "-c", "docker compose ps --format '{{.Service}} {{.Status}}'"], cwd=SRV,
                           env=env_srv(), capture_output=True, text=True)
        if r.stdout.count("Up") >= 3 and "starting" not in r.stdout:
            break
        time.sleep(2)
    time.sleep(3)
    srv(rec, "docker compose ps --format 'table {{.Service}}\\t{{.Image}}\\t{{.Status}}'")


def d04(rec):
    srv(rec, CURL_API + "; echo")
    r = subprocess.run(["bash", "-c", "docker compose exec -T vibe vibe setup"], cwd=SRV, env=env_srv(),
                       capture_output=True, text=True, timeout=60)
    m = re.search(r"\b(vmsc_[A-Za-z0-9_\-]+)", r.stdout + r.stderr)
    if r.returncode != 0 or not m:
        raise SystemExit(f"d04: vibe setup exited {r.returncode} without a credential")
    p = f"{T}/kc/setup-credential"
    fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, m.group(1).encode())
    os.close(fd)
    rec.note(PROMPT_SRV, "docker compose exec -T vibe vibe setup", r.stdout + r.stderr)


def d05(rec):
    os.makedirs(f"{HOME}/.local/opt", exist_ok=True)
    gl = f"{HOME}/.local/opt/git-env"
    if not os.path.lexists(gl):
        os.symlink(os.path.expanduser("~/.local/opt/git-env"), gl)
    subprocess.run(["cp", f"{T}/pki/ca.pem", f"{HOME}/acme-ca.pem"], check=True)
    a = "vibe-0.1.0-x86_64-unknown-linux-gnu"
    tool(rec, f"curl -fsSLO {REL}/{a}.tar.gz && curl -fsSLO {REL}/SHA256SUMS", shown=f"curl -fsSLO {REL}/{a}.tar.gz\ncurl -fsSLO {REL}/SHA256SUMS")
    tool(rec, "sha256sum --check --ignore-missing SHA256SUMS")
    tool(rec, f"tar -xzf {a}.tar.gz && mkdir -p ~/.local/bin && cp {a}/vibe ~/.local/bin/vibe",
         shown=f"tar -xzf {a}.tar.gz\nmkdir -p ~/.local/bin\ncp {a}/vibe ~/.local/bin/vibe")
    tool(rec, "vibe --version")
    tool(rec, "true", shown="export VIBE_CA_FILE=~/acme-ca.pem   # Acme's own certificate authority")
    subprocess.run(["bash", "-c", 'git config --global user.name "Ada Lovelace" && git config --global user.email ada@acme.test && git config --global init.defaultBranch main'],
                   env=env_tool(), check=True)


def d06(rec):
    cmd = (f"vibe admin connect-oidc {WHERE} \\\n    --issuer https://idp.{D}:{IDP}/realms/acme \\\n"
           "    --client-id vibe \\\n    --administrators-group vibe-administrators")
    script = ("read -rs VIBE_SETUP_CREDENTIAL && export VIBE_SETUP_CREDENTIAL\n"
              "read -rs VIBE_OIDC_CLIENT_SECRET && export VIBE_OIDC_CLIENT_SECRET\n" + cmd)
    tool(rec, script, stdin=f"{secret('setup-credential')}\n{secret('client-secret')}\n")
    c = f"curl -s --cacert ~/acme-ca.pem --resolve api.{D}:{PORT}:127.0.0.1 https://api.{D}:{PORT}/v1/installation"
    tool(rec, c + "; echo", shown=c)


def d07(rec):
    f = ff("ada")
    f.go(f"https://dashboard.{D}:{PORT}/")
    time.sleep(2)
    provider_sign_in(rec, f, "ada")
    rec.shot(f, "ada-dashboard", marks=["[data-vm-user]"])


def d08(rec):
    log = open(f"{T}/signin.out", "w")
    p = subprocess.Popen(as_laptop(f"vibe sign-in {WHERE} --no-browser"), cwd=HOME, env=env_tool(),
                         stdout=log, stderr=subprocess.STDOUT, text=True)
    addr = code = None
    for _ in range(60):
        time.sleep(0.5)
        t = open(f"{T}/signin.out").read()
        a, c = re.search(r"https://\S+", t), re.search(r"check code: *(\S+)", t)
        if a and c:
            addr, code = a.group(0), c.group(1)
            break
    if not addr:
        p.kill()
        raise SystemExit("d08: vibe sign-in printed no address")
    rec.note(ptool(HOME), f"vibe sign-in {WHERE} --no-browser", open(f"{T}/signin.out").read())
    f = ff("ada")
    f.go(addr)
    time.sleep(1.5)
    provider_sign_in(rec, f, "ada")
    rec.shot(f, "confirm", marks=["form"])
    box = [e for e in f.find("form input") if f.attr(e, "type") == "text"]
    f.type(box[0], code)
    rec.shot(f, "confirm-typed", marks=["form button"])
    f.click(f.find("form button")[0])
    time.sleep(1.5)
    rec.shot(f, "confirmed")
    p.wait(timeout=60)
    rec.note(ptool(HOME), f"vibe sign-in {WHERE} --no-browser", open(f"{T}/signin.out").read())
    rec.steps.pop(0)  # the first note was only the address; the second holds the whole output
    tool(rec, "vibe whoami")


def d09(rec):
    tool(rec, "vibe new hello")
    tool(rec, "npm install --no-fund --no-audit", cwd=f"{HOME}/hello", shown="npm install", timeout=600)
    tool(rec, "vibe check", cwd=f"{HOME}/hello")


def d10(rec):
    hello = f"{HOME}/hello"
    log = open(f"{T}/dev.out", "w")
    p = subprocess.Popen(as_laptop("exec vibe dev"), cwd=hello, env=env_tool(), stdout=log, stderr=subprocess.STDOUT, text=True)
    open(f"{T}/run/vibe-dev.pid", "w").write(str(p.pid))
    try:
        for _ in range(600):
            time.sleep(0.5)
            if "serves" in open(f"{T}/dev.out").read():
                break
        f = ff("ada")
        f.go("http://hello.localhost:7480/")
        time.sleep(1.5)
        rec.shot(f, "dev-users", marks=["form button, form input[type=submit]"])
        b = f.find("form button, form input[type=submit]")
        f.click(b[0])
        time.sleep(1.5)
        rec.shot(f, "dev-page")
        time.sleep(0.5)
        rec.note(ptool(hello), "vibe dev", open(f"{T}/dev.out").read())
        rec.steps.insert(0, rec.steps.pop())  # the terminal first, then the browser
    finally:
        p.send_signal(2)
        try:
            p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            p.kill()
        os.remove(f"{T}/run/vibe-dev.pid")


def d11(rec):
    h = f"{HOME}/hello"
    tool(rec, "git init -q && git add --all && git commit -q -m 'The first version' && git log --oneline",
         cwd=h, shown='git init\ngit add --all\ngit commit -m "The first version"')
    tool(rec, "vibe deploy", cwd=h, timeout=600)


def d12(rec):
    f = ff("ada")
    f.go(f"https://hello.{D}:{PORT}/")
    time.sleep(2)
    provider_sign_in(rec, f, "ada")
    rec.shot(f, "hello-ada")
    f.go(f"https://dashboard.{D}:{PORT}/")
    time.sleep(1.5)
    rec.shot(f, "dashboard-ada", marks=['[data-vm-application="hello"]'])


def d13(rec):
    f = ff("grace")
    f.go(f"https://dashboard.{D}:{PORT}/")
    time.sleep(2)
    provider_sign_in(rec, f, "grace")
    rec.shot(f, "grace-dashboard-empty", marks=["[data-vm-user]"])
    f.go(f"https://hello.{D}:{PORT}/")
    time.sleep(2)
    rec.shot(f, "grace-refused", marks=["main, body"])


def d14(rec):
    tool(rec, "vibe share hello --email grace@acme.test", cwd=f"{HOME}/hello")


def d15(rec):
    f = ff("grace")
    f.go(f"https://dashboard.{D}:{PORT}/")
    time.sleep(1.5)
    rec.shot(f, "grace-dashboard", marks=['[data-vm-application="hello"]'])
    f.click(f.find('[data-vm-application="hello"] a[data-vm-open]')[0])
    time.sleep(2)
    rec.shot(f, "grace-hello")


ORDER = ["d02", "d03", "d04", "d05", "d06", "d07", "d08", "d09", "d10", "d11", "d12", "d13", "d14", "d15"]


def main():
    os.makedirs(f"{T}/run", exist_ok=True)
    os.makedirs(SRV, exist_ok=True)
    os.makedirs(HOME, exist_ok=True)
    for s in (sys.argv[1:] or ORDER):
        rec = Rec(s)
        globals()[s](rec)
        rec.save()


if __name__ == "__main__":
    main()
