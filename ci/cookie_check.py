#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
"""Local server for the cookie persistence check of the Windows smoke test (run 4c of
ci/smoke-windows.ps1, "Stay signed in").

GET / without our cookie sets a persistent cookie (`mbsmoke`, Max-Age one day) and a session
cookie (`mbsess`, no expiry) and answers with the title "cookie SET <value>"; with the cookie
it answers "cookie KEPT <value> session=<1|0>". Each request is appended to --log as
"GET / cookie=<value|-> session=<1|0>". Test values only, nothing else is read.

usage: cookie_check.py --port 18082 --log <file>
"""
import argparse
import http.cookies
import http.server
import secrets


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18082)
    ap.add_argument("--log", required=True)
    a = ap.parse_args()

    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path not in ("/", "/index.html"):
                self.send_error(404)
                return
            jar = http.cookies.SimpleCookie()
            try:
                jar.load(self.headers.get("Cookie") or "")
            except http.cookies.CookieError:
                pass
            have = jar["mbsmoke"].value if "mbsmoke" in jar else None
            sess = "1" if "mbsess" in jar else "0"
            with open(a.log, "a") as f:
                f.write(f"GET / cookie={have or '-'} session={sess}\n")
            self.send_response(200)
            if have:
                title = f"cookie KEPT {have} session={sess}"
            else:
                have = secrets.token_hex(8)
                title = f"cookie SET {have}"
                self.send_header("Set-Cookie", f"mbsmoke={have}; Max-Age=86400; Path=/; HttpOnly")
                self.send_header("Set-Cookie", f"mbsess={have}; Path=/; HttpOnly")
            body = f"<!doctype html><title>{title}</title><p>{title}</p>".encode()
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, fmt, *args):
            pass

    http.server.ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()


if __name__ == "__main__":
    main()
