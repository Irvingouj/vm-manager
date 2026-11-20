"""A small Marionette client: drives a real Firefox (headless). From the by-hand
walk of ticket 68, with rect() added. Usage as a module: from ff import Firefox."""
import base64, json, socket

class Firefox:
    def __init__(self, port=2830):
        self.s = socket.create_connection(("127.0.0.1", port), timeout=60)
        self.n = 0
        self._read()  # hello
        self.cmd("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {"acceptInsecureCerts": False}}})

    def _read(self):
        size = b""
        while True:
            c = self.s.recv(1)
            if c == b":":
                break
            size += c
        need, data = int(size), b""
        while len(data) < need:
            data += self.s.recv(need - len(data))
        return json.loads(data)

    def cmd(self, name, params=None):
        self.n += 1
        body = json.dumps([0, self.n, name, params or {}])
        self.s.sendall(f"{len(body)}:{body}".encode())
        kind, n, error, result = self._read()
        if error:
            raise RuntimeError(f"{name}: {error.get('error')}: {error.get('message')}")
        return result

    def go(self, url):
        self.cmd("WebDriver:Navigate", {"url": url})

    def url(self):
        return self.cmd("WebDriver:GetCurrentURL")["value"]

    def title(self):
        return self.cmd("WebDriver:GetTitle")["value"]

    def find(self, css):
        found = self.cmd("WebDriver:FindElements", {"using": "css selector", "value": css})
        return [next(iter(e.values())) for e in found]

    def text(self, element):
        return self.cmd("WebDriver:GetElementText", {"id": element})["value"]

    def attr(self, element, name):
        return self.cmd("WebDriver:GetElementAttribute", {"id": element, "name": name})["value"]

    def click(self, element):
        self.cmd("WebDriver:ElementClick", {"id": element})

    def type(self, element, text):
        self.cmd("WebDriver:ElementSendKeys", {"id": element, "text": text})

    def rect(self, element):
        r = self.cmd("WebDriver:GetElementRect", {"id": element})
        return [r["x"], r["y"], r["width"], r["height"]]

    def size(self, width, height):
        self.cmd("WebDriver:SetWindowRect", {"width": width, "height": height})

    def shot(self, path, full=True):
        data = self.cmd("WebDriver:TakeScreenshot", {"full": full})["value"]
        open(path, "wb").write(base64.b64decode(data))

    def js(self, script):
        return self.cmd("WebDriver:ExecuteScript", {"script": script, "args": []})["value"]

    def quit(self):
        try:
            self.cmd("Marionette:Quit", {"flags": ["eForceQuit"]})
        except Exception:
            pass

    def close(self):
        self.cmd("WebDriver:DeleteSession")

    def chrome(self, script):
        self.cmd("Marionette:SetContext", {"value": "chrome"})
        try:
            return self.cmd("WebDriver:ExecuteScript", {"script": script, "args": []})["value"]
        finally:
            self.cmd("Marionette:SetContext", {"value": "content"})
