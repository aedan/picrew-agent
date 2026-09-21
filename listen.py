#!/usr/bin/env python3
"""PiCrew listen agent: the hub dials this box. Nothing here phones home."""
import json, os, socket, subprocess, urllib.request, urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = os.environ.get("PICREW_TOKEN", "")
NAME = os.environ.get("PICREW_AGENT_NAME", socket.gethostname())
OPENCODE = os.environ.get("OPENCODE_URL", "http://127.0.0.1:4096").rstrip("/")
PROJECTS = os.environ.get("PICREW_PROJECTS", "/srv/projects")
PORT = int(os.environ.get("PICREW_AGENT_PORT", "5280"))


def repos():
    out = []
    root = (PROJECTS.split(",")[0].strip() or "/srv/projects")
    try:
        os.makedirs(root, exist_ok=True)
        for n in os.listdir(root):
            p = os.path.join(root, n)
            if os.path.isdir(p) and not n.startswith("."):
                out.append(p)
    except OSError:
        pass
    return out


class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print("picrew-agent", fmt % args)

    def _token(self):
        got = (self.headers.get("X-PiCrew-Token") or "").strip()
        auth = self.headers.get("Authorization") or ""
        if auth.lower().startswith("bearer "):
            got = auth[7:].strip()
        return got

    def _auth(self):
        if not TOKEN:
            return False
        return self._token() == TOKEN

    def _json(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if not self._auth():
            return self._json(401, {"ok": False, "error": "token"})
        path = self.path.split("?")[0]
        if path == "/healthz":
            return self._json(200, {"ok": True, "name": NAME})
        if path == "/info":
            return self._json(
                200,
                {
                    "ok": True,
                    "name": NAME,
                    "repos": repos(),
                    "opencode": OPENCODE,
                    "hostname": socket.gethostname(),
                },
            )
        return self._json(404, {"ok": False, "error": "not found"})

    def do_POST(self):
        if not self._auth():
            return self._json(401, {"ok": False, "error": "token"})
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b"{}"
        try:
            body = json.loads(raw.decode() or "{}")
        except json.JSONDecodeError:
            return self._json(400, {"ok": False, "error": "bad json"})
        path = self.path.split("?")[0]
        if path == "/rpc":
            return self._rpc(body)
        return self._json(404, {"ok": False, "error": "not found"})

    def _rpc(self, body):
        kind = body.get("kind")
        if kind == "opencode":
            return self._opencode(body)
        if kind == "exec":
            return self._exec(body.get("payload") or {})
        if kind == "config":
            return self._config(body.get("payload") or {})
        return self._json(400, {"ok": False, "error": "unknown kind"})

    def _opencode(self, body):
        method = (body.get("method") or "GET").upper()
        at = body.get("path") or "/"
        if not at.startswith("/"):
            at = "/" + at
        url = OPENCODE + at
        data = None
        headers = {"Accept": "application/json"}
        if body.get("body") is not None:
            data = json.dumps(body["body"]).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=float(body.get("timeout") or 120)) as res:
                raw = res.read()
                try:
                    parsed = json.loads(raw.decode())
                except Exception:
                    parsed = raw.decode("utf-8", "replace")
                return self._json(200, {"ok": True, "status": res.status, "body": parsed})
        except urllib.error.HTTPError as e:
            raw = e.read()
            return self._json(
                200,
                {"ok": False, "status": e.code, "body": raw.decode("utf-8", "replace")},
            )
        except Exception as e:
            return self._json(502, {"ok": False, "error": str(e)})

    def _exec(self, p):
        cmd = p.get("command")
        args = p.get("args") or []
        cwd = p.get("cwd") or "/"
        env = os.environ.copy()
        if isinstance(p.get("env"), dict):
            env.update({str(k): str(v) for k, v in p["env"].items()})
        timeout = float(p.get("timeoutMs") or 60000) / 1000.0
        try:
            r = subprocess.run(
                [cmd, *args],
                cwd=cwd,
                env=env,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            return self._json(
                200,
                {
                    "ok": True,
                    "body": {
                        "code": r.returncode,
                        "stdout": r.stdout[-20000:],
                        "stderr": r.stderr[-20000:],
                        "repos": repos(),
                    },
                },
            )
        except Exception as e:
            return self._json(502, {"ok": False, "error": str(e)})

    def _config(self, p):
        cfg = p.get("opencodeConfig")
        path = os.path.expanduser("~/.config/opencode/opencode.json")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        if cfg is not None:
            with open(path, "w") as f:
                json.dump(cfg, f, indent=2)
        return self._json(200, {"ok": True})


if __name__ == "__main__":
    os.makedirs(PROJECTS.split(",")[0].strip() or "/srv/projects", exist_ok=True)
    httpd = ThreadingHTTPServer(("0.0.0.0", PORT), H)
    print("picrew-agent listening on 0.0.0.0:%s name=%s" % (PORT, NAME))
    httpd.serve_forever()
